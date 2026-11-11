//! The chat as a pixel screen draws it (1145): the conversation list's rows
//! and the open conversation's bubbles, grouped and dated, the avatars'
//! letters and colours, and the colour and size tokens of
//! `docs/src/concepts/loquax-ui.md`, section "Pixel screens". The phone's
//! facade (`loquax-android`) and the desktop window (`loquax gui`) both
//! draw from here, so a bubble, a mark or a group means the same on each.
//!
//! The terminal draws the same model through [`super::layout`]; it has
//! cells, not bubbles, and reads none of this.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use leviculum_lxmf::constants::FIELD_ICON_APPEARANCE;
use leviculum_lxmf::msgpack;
use leviculum_lxmf::Field;
use leviculum_ui::delivery::DeliveryState;
use leviculum_ui::pixel;

use super::liveness::Liveness;
use super::reactions;
use super::replies::Quoted;
use super::strings::{Arg, Lang, StrId, Text};
use super::{Conversation, Message, UiModel};
use crate::telemetry::Fix;
use crate::when::WallTime;

/// Messages of one sender this close together form a group.
pub const GROUP_SECS: i64 = 180;

/// The marks a pixel screen draws, the ladder of
/// `docs/src/concepts/loquax-read-receipts.md`, section 6 (1232), from the
/// glyph table's ten states: a clock while nobody confirmed it, one tick
/// from the mailbox or the recipient's proof, two once the recipient's
/// device said it has it (`lev` 0x07, 1462), two heavier in the accent once
/// read, a red circle with `!` when it was not sent. Seven marks, five
/// glyphs: sending and sent-unconfirmed share the clock, mailbox and
/// arrived the one tick, and each pair differs in its words.
///
/// The proof is one tick, never two: the recipient's mailbox signs it with
/// the recipient's identity as soon as it archived the message, so the
/// writer cannot tell it from a device's (1462). Only the device's own
/// word is two.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    /// Queued, stamping or in flight.
    Sending,
    /// Sent with no proof back: it left this device, nobody confirmed it.
    Unconfirmed,
    /// The mailbox took it (a propagated `Sent`).
    Mailbox,
    /// The recipient's identity proved it: its mailbox or its device has
    /// it, the proof does not say which.
    Arrived,
    /// The recipient's device said it has it (`lev` 0x07).
    Delivered,
    /// The peer's read receipt named it (1230).
    Read,
    /// Rejected or given up: its mark sends it again.
    Failed,
}

/// `state` as the screen shows it ([`super::shown_delivery`]).
pub(crate) fn shown<B>(state: Option<DeliveryState>, model: &UiModel<B>) -> Option<DeliveryState> {
    state.map(|state| super::shown_delivery(state, model.home_client, model.reading.on))
}

/// Every mark, in the ladder's order.
pub const MARKS: [Mark; 7] = [
    Mark::Sending,
    Mark::Unconfirmed,
    Mark::Mailbox,
    Mark::Arrived,
    Mark::Delivered,
    Mark::Read,
    Mark::Failed,
];

impl Mark {
    pub fn of(state: Option<DeliveryState>) -> Self {
        match state {
            None
            | Some(DeliveryState::Queued)
            | Some(DeliveryState::Stamping)
            | Some(DeliveryState::InFlight) => Self::Sending,
            Some(DeliveryState::Sent) => Self::Unconfirmed,
            Some(DeliveryState::InMailbox) => Self::Mailbox,
            Some(DeliveryState::Arrived) => Self::Arrived,
            Some(DeliveryState::Delivered) => Self::Delivered,
            Some(DeliveryState::Read { .. }) => Self::Read,
            Some(DeliveryState::Rejected) | Some(DeliveryState::Failed) => Self::Failed,
        }
    }

    /// The facade's word: `sending`, `unconfirmed`, `mailbox`,
    /// `arrived`, `delivered`, `read`, `failed`.
    pub fn word(self) -> &'static str {
        match self {
            Self::Sending => "sending",
            Self::Unconfirmed => "unconfirmed",
            Self::Mailbox => "mailbox",
            Self::Arrived => "arrived",
            Self::Delivered => "delivered",
            Self::Read => "read",
            Self::Failed => "failed",
        }
    }

    /// The name the phone gives the mark's view (its `R.id`), which the
    /// device walk reads off the screen.
    pub fn resource(self) -> &'static str {
        match self {
            Self::Sending | Self::Unconfirmed => "state_clock",
            Self::Mailbox | Self::Arrived => "state_tick",
            Self::Delivered => "state_ticks",
            Self::Read => "state_ticks_read",
            Self::Failed => "state_failed",
        }
    }

    /// Its drawing on the [`GRID`].
    pub fn shape(self) -> &'static Shape {
        match self {
            Self::Sending | Self::Unconfirmed => &CLOCK,
            Self::Mailbox | Self::Arrived => &TICK,
            Self::Delivered => &TICKS,
            Self::Read => &TICKS_READ,
            Self::Failed => &FAILED,
        }
    }
}

// The grid, the circle and the shape a mark is drawn as are the shared
// glyph vocabulary (`leviculum_ui::glyph`), which the window's widgets in
// leviculum-ui-iced draw without knowing a mark.
pub use leviculum_ui::glyph::{Circle, Shape, GRID};

/// The clock: a ring and its hands.
pub const CLOCK: Shape = Shape {
    disc: None,
    ring: Some(Circle {
        x: 12.0,
        y: 12.0,
        r: 9.0,
    }),
    lines: &[&[(12.0, 7.0), (12.0, 12.0), (15.5, 14.0)]],
    width: 1.8,
    dot: None,
};

/// One tick.
pub const TICK: Shape = Shape {
    disc: None,
    ring: None,
    lines: &[&[(5.0, 12.5), (9.5, 17.0), (19.0, 7.0)]],
    width: 2.0,
    dot: None,
};

/// The first of two ticks, and the second set 6.5 units right of it, its
/// short leg cut where it would run into the first's long one: the gap
/// between the long legs is 4.7 units less the stroke, and the cut keeps
/// the second clear of the first even at read's heavier stroke.
const TWO_TICKS: &[&[(f32, f32)]] = &[
    &[(1.5, 12.5), (6.0, 17.0), (15.5, 7.0)],
    &[(11.6, 16.1), (12.5, 17.0), (22.0, 7.0)],
];

/// Two ticks: delivered.
pub const TICKS: Shape = Shape {
    disc: None,
    ring: None,
    lines: TWO_TICKS,
    width: 2.0,
    dot: None,
};

/// Two ticks, heavier: read.
pub const TICKS_READ: Shape = Shape {
    disc: None,
    ring: None,
    lines: TWO_TICKS,
    width: 2.6,
    dot: None,
};

/// A disc with an exclamation mark: not sent.
pub const FAILED: Shape = Shape {
    disc: Some(Circle {
        x: 12.0,
        y: 12.0,
        r: 10.0,
    }),
    ring: None,
    lines: &[&[(12.0, 6.5), (12.0, 13.0)]],
    width: 2.4,
    dot: Some(Circle {
        x: 12.0,
        y: 17.0,
        r: 1.4,
    }),
};

/// The colour mark `mark` is drawn in on our bubble, and its sign's ground
/// for [`Mark::Failed`]: the meta colour, the read mark, the failed mark.
pub fn bubble_ink(mark: Mark, colours: &Colours) -> u32 {
    match mark {
        Mark::Read => colours.read_mark,
        Mark::Failed => colours.failed_mark,
        _ => colours.bubble_ours_meta,
    }
}

/// The colour mark `mark` is drawn in on the list's ground.
pub fn list_ink(mark: Mark, colours: &Colours) -> u32 {
    match mark {
        Mark::Read => colours.accent,
        Mark::Failed => colours.list_failed_mark,
        _ => colours.text_secondary,
    }
}

/// The initials for a peer (1129): the first letter of the name's first
/// word and of its last, one letter for a name of one word, and `?` for a
/// peer without a name (1452: never hex; its colour, from the address,
/// tells two of them apart).
pub fn initials(name: Option<&str>, _peer: &[u8; 16]) -> String {
    let first_letter = |word: &str| {
        word.chars()
            .find(|c| c.is_alphanumeric())
            .or_else(|| word.chars().next())
    };
    let words: Vec<&str> = name
        .map(|name| name.split_whitespace().collect())
        .unwrap_or_default();
    let letters: Vec<char> = match words.as_slice() {
        [] => Vec::new(),
        [only] => first_letter(only).into_iter().collect(),
        [first, .., last] => first_letter(first)
            .into_iter()
            .chain(first_letter(last))
            .collect(),
    };
    if letters.is_empty() {
        return "?".to_string();
    }
    letters.into_iter().flat_map(char::to_uppercase).collect()
}

/// The foreground and background of an LXMF icon appearance in `fields`,
/// as `rrggbb`: Sideband's and Columba's `[icon name, fg, bg]` with three
/// bytes each. None for no such field or one of another shape.
pub fn appearance(fields: &[Field]) -> Option<(String, String)> {
    let (_, value) = fields.iter().find(|(k, _)| *k == FIELD_ICON_APPEARANCE)?;
    let mut p = 0;
    if msgpack::array_len(value, &mut p).ok()? < 3 {
        return None;
    }
    msgpack::skip(value, &mut p).ok()?;
    let mut colour = || {
        let bytes = msgpack::read_bin(value, &mut p).ok()?;
        (bytes.len() == 3).then(|| crate::address::to_hex(bytes))
    };
    let fg = colour()?;
    let bg = colour()?;
    Some((fg, bg))
}

/// The newest icon appearance among a conversation's received messages.
pub fn latest_appearance(messages: &[Message]) -> Option<(String, String)> {
    messages
        .iter()
        .rev()
        .filter(|message| !message.outbound)
        .find_map(|message| appearance(&message.fields))
}

/// The avatar's ground when the peer sent no icon appearance: one of eight
/// picked by the address's first byte, each under white letters at 5:1 or
/// more (the contrast table of 1129).
pub const AVATAR_PALETTE: [u32; 8] = [
    0x8e4b8a, 0x3d6fb6, 0xb0503a, 0x2f7a6e, 0x7a5fb8, 0xa2526a, 0x5b6b2f, 0x8a6420,
];

/// The avatar's letters and ground, `0xrrggbb` each: the icon appearance's
/// when it is well formed, else white on the address's palette entry.
pub fn avatar_colours(peer: &[u8; 16], icon: Option<&(String, String)>) -> (u32, u32) {
    let parse = |hex: &str| u32::from_str_radix(hex, 16).ok().filter(|_| hex.len() == 6);
    if let Some((fg, bg)) = icon {
        if let (Some(fg), Some(bg)) = (parse(fg), parse(bg)) {
            return (fg, bg);
        }
    }
    (
        0xffffff,
        AVATAR_PALETTE[usize::from(peer[0]) % AVATAR_PALETTE.len()],
    )
}

/// What a conversation's last message is, for the list's words: `text`,
/// `position` or `reaction` (1130), the same split as the English line.
pub fn kind(message: &Message) -> &'static str {
    if message.is_reaction() {
        "reaction"
    } else if message.is_position() {
        "position"
    } else if super::photos::photo_of(&message.fields).is_some() {
        "photo"
    } else if !super::files::files_of(&message.fields).is_empty() {
        "file"
    } else if super::voice::voice_of(&message.fields).is_some() {
        "voice"
    } else {
        "text"
    }
}

/// The list's line for a reaction someone else sent, with its subject
/// (1447, walk 1 finding 24): "Lew Mobile hat mit ❤️ reagiert", where the
/// line said "hat mit ❤️ reagiert" and nobody knew who. In a group the
/// subject is the member whose copy it is; `None` where no name is known,
/// and for our own reaction, whose line says "Du" already.
pub fn reaction_line<B>(model: &UiModel<B>, peer: &[u8; 16], message: &Message) -> Option<String> {
    let (_, content) = message.reaction().filter(|_| !message.outbound)?;
    let name = match model.group(peer) {
        Some(info) => {
            let (address, name, _) =
                super::groups::copy_of(&message.body, &message.fields, Some(info))?;
            match model.names.shown(&address) {
                Some(shown) => shown.to_string(),
                None if !name.is_empty() => name.to_string(),
                None => info
                    .member(&address)
                    .filter(|m| !m.name.is_empty())
                    .map(|m| m.name.clone())?,
            }
        }
        None => model.names.shown(peer)?.to_string(),
    };
    Some(
        Text::Key(
            StrId::ReactedBy,
            Vec::from([Arg::Str(name), Arg::Str(content)]),
        )
        .resolve(model.lang),
    )
}

/// The list's line for a conversation's last message, in `lang`: what
/// the phone's notification says too, so a word Loquax puts there itself
/// is the catalogue's (1377), never an English one under a German list.
pub fn preview(message: &Message, lang: Lang) -> String {
    if let Some((_, content)) = message.reaction() {
        let id = match message.outbound {
            true => StrId::YouReacted,
            false => StrId::Reacted,
        };
        return Text::Key(id, Vec::from([Arg::Str(content)])).resolve(lang);
    }
    if message.is_report() {
        return StrId::RowPositionReport.text(lang).to_string();
    }
    if message.is_position() {
        return StrId::RowPositionShared.text(lang).to_string();
    }
    if message.body.trim().is_empty() && super::photos::photo_of(&message.fields).is_some() {
        return StrId::RowPhoto.text(lang).to_string();
    }
    if message.body.trim().is_empty() {
        if let Some(file) = super::files::files_of(&message.fields).into_iter().next() {
            return file.name;
        }
        if let Some(voice) = super::voice::voice_of(&message.fields) {
            return Text::Key(
                StrId::Voice,
                Vec::from([Arg::Str(super::voice::duration_words(voice.duration_ms))]),
            )
            .resolve(lang);
        }
    }
    // A relay's post (1482): its writer and the first line of its words,
    // not the writer's name alone.
    if let Some(writer) = leviculum_lxmf::relay::prefix(&message.body).filter(|_| !message.outbound)
    {
        let first = writer.text.lines().next().unwrap_or_default();
        return alloc::format!("{}: {first}", writer.name);
    }
    message.body.lines().next().unwrap_or_default().to_string()
}

/// The list's line for an invitation: the card's title in the reader's
/// language ("Anna lädt dich zu Verein ein"), not the English sentence
/// the wire carries for stock clients with the group's code in it (1377).
pub(crate) fn invitation_line<B>(model: &UiModel<B>, message: &Message) -> Option<String> {
    let group = leviculum_lxmf::lev::Lev::from_fields(&message.fields)?
        .invite?
        .group;
    let card = super::group_scene::invite(model, &group, message.outbound)?;
    Some(card.title.resolve(model.lang))
}

/// One row of the conversation list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListRow {
    pub peer: [u8; 16],
    /// As every loquax screen names the peer.
    pub name: String,
    /// The last message's line ([`preview`]).
    pub last: String,
    /// Whether that message is ours.
    pub ours: bool,
    pub at: Option<WallTime>,
    /// Unread rows: a position report counts nothing (1209).
    pub unread: usize,
    /// How many reports the last row folds when it is a run of them, else 0
    /// (1184).
    pub updates: usize,
    pub initials: String,
    /// The peer's own avatar colours, `rrggbb`.
    pub icon: Option<(String, String)>,
    /// [`kind`] of the last message, empty for none.
    pub kind: &'static str,
    /// The reaction's content when the last message is one.
    pub reaction: Option<String>,
    /// Ours only: the last message's delivery, drawn where an unread count
    /// would be.
    pub mark: Option<Mark>,
    /// Ours only, when [`Mark::Read`]: the reader's time, unix seconds,
    /// for the words a screen reader says (1232).
    pub read_at: Option<u32>,
    pub pinned: bool,
    pub muted: bool,
    /// Whether the peer's position is alive (1209): the avatar's ring.
    pub liveness: Liveness,
    /// What was typed and not sent, on one line (1219): shown as the last
    /// line, `Draft: …`, in italics, while the conversation is not the open
    /// one. It counts as nothing unread and moves nothing in the list.
    pub draft: Option<String>,
}

/// The conversation list as the model orders it.
pub fn list_rows<B>(model: &UiModel<B>) -> Vec<ListRow> {
    rows(model, model.order())
}

/// The rows for the conversations `order` names, in its order.
pub fn rows<B>(model: &UiModel<B>, order: Vec<usize>) -> Vec<ListRow> {
    order
        .into_iter()
        .map(|index| list_row(model, &model.conversations[index].peer))
        .collect()
}

/// The row for `peer`, empty when it has no conversation: a blocked peer
/// is listed whether it wrote or not.
pub fn list_row<B>(model: &UiModel<B>, peer: &[u8; 16]) -> ListRow {
    model.count(super::counters::Op::RowsBuilt, 1);
    let conversation = model.conversations.iter().find(|c| c.peer == *peer);
    // Nothing loaded: the store's row answers (1490, schema 17), its
    // liveness and its position words judged fresh at this clock.
    if let Some((c, listing)) = conversation
        .filter(|c| c.messages.is_empty())
        .and_then(|c| Some((c, c.listing.as_ref()?)))
    {
        let liveness = model.liveness(peer);
        let now = model.now.map(|now| now.unix_secs);
        return ListRow {
            peer: *peer,
            name: model.names.label(peer, model.lang),
            last: match listing.kind {
                super::listing::LastKind::Position => {
                    format!("{PIN} {}", liveness.list_words(now))
                }
                _ => listing.line.clone(),
            },
            ours: listing.ours,
            at: listing.at,
            unread: listing.unread_rows as usize,
            updates: listing.updates as usize,
            initials: initials(model.names.shown(peer), peer),
            icon: listing.appearance.clone(),
            kind: listing.kind.word(),
            reaction: listing.reaction.clone(),
            mark: listing.mark,
            read_at: listing.read_at,
            pinned: c.pinned,
            muted: model.muted(peer),
            liveness,
            draft: Some(c)
                .filter(|c| model.conversations.get(model.selected).map(|o| o.peer) != Some(c.peer))
                .and_then(|c| c.draft.line()),
        };
    }
    let messages = conversation.map_or(&[][..], |c| c.messages.as_slice());
    // A group's state (1307) is no line of the list.
    let last = messages.iter().rev().find(|m| !m.is_group_state());
    // A run of reports is one line, and it says whether the position is
    // alive (1209), not how many came.
    let updates = conversation
        .filter(|_| last.is_some_and(Message::is_report))
        .map_or(0, |c| c.run(messages.len().saturating_sub(1)).len());
    let liveness = model.liveness(peer);
    let now = model.now.map(|now| now.unix_secs);
    ListRow {
        peer: *peer,
        name: model.names.label(peer, model.lang),
        last: last
            .map(|m| match m.is_report() {
                true => format!("{PIN} {}", liveness.list_words(now)),
                false => invitation_line(model, m)
                    .or_else(|| reaction_line(model, peer, m))
                    .unwrap_or_else(|| preview(m, model.lang)),
            })
            .unwrap_or_default(),
        ours: last.is_some_and(|m| m.outbound),
        at: last.and_then(|m| m.at),
        unread: conversation.map_or(0, |c| c.unread_rows()),
        updates,
        initials: initials(model.names.shown(peer), peer),
        icon: conversation.and_then(Conversation::appearance),
        kind: last.map_or("", kind),
        reaction: last
            .filter(|m| m.is_reaction())
            .and_then(|m| m.reaction())
            .map(|(_, content)| content),
        mark: last
            .filter(|m| m.outbound)
            .map(|m| Mark::of(shown(m.delivery, model))),
        read_at: last
            .filter(|m| m.outbound)
            .and_then(|m| match shown(m.delivery, model) {
                // 0 is a read at no known time (1506): no time to say.
                Some(DeliveryState::Read { at }) if at > 0 => Some(at),
                _ => None,
            }),
        pinned: conversation.is_some_and(|c| c.pinned),
        muted: model.muted(peer),
        liveness,
        draft: conversation
            .filter(|c| model.conversations.get(model.selected).map(|o| o.peer) != Some(c.peer))
            .and_then(|c| c.draft.line()),
    }
}

/// The avatar's ring for a peer whose position is `state` (1209): the
/// accent while live, the secondary text colour once it stopped, none when
/// stale. Every screen rings the same: the list, the chat's bar, the
/// Friends sheet and the map's pin.
pub fn ring(state: super::liveness::State, colours: &Colours) -> Option<u32> {
    super::scene::Ring::of(state).map(|ring| ring.colour(colours))
}

/// The list's mark for a report's line: `📍 live`.
pub const PIN: &str = "\u{1f4cd}";

/// Whether `row` is found by the list's search `query`: every word of it,
/// case folded, in the name or the last line. The empty query finds all.
pub fn row_matches(row: &ListRow, query: &str) -> bool {
    let name = row.name.to_lowercase();
    let last = row.last.to_lowercase();
    query
        .split_whitespace()
        .map(str::to_lowercase)
        .all(|word| name.contains(&word) || last.contains(&word))
}

/// A reaction under a bubble.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chip {
    pub content: String,
    pub count: usize,
    pub ours: bool,
}

/// One bubble: a message of the conversation that is not a reaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bubble {
    /// The message's key on this device ([`Message::activity`]).
    pub key: u64,
    /// The LXMF id; none for a send not yet packed.
    pub id: Option<[u8; 32]>,
    pub ours: bool,
    /// What the bubble says: a report with a position is its position line
    /// alone, so empty, and one without says so in words.
    pub body: String,
    /// Ours only.
    pub mark: Option<Mark>,
    /// Ours only, when [`Mark::Read`]: the reader's time, unix seconds
    /// (1230).
    pub read_at: Option<u32>,
    /// Ours only, when [`Mark::Delivered`] came from the propagation
    /// network and not a proof (1395): the words differ, the ticks do not.
    pub witness: Option<super::arrival::Witness>,
    pub at: Option<WallTime>,
    pub chips: Vec<Chip>,
    /// The position the message carries, which the bubble's line opens the
    /// map on.
    pub fix: Option<Fix>,
    /// A reply's quote.
    pub quote: Option<Quoted>,
    /// The reports a run's row stands for, it the newest, when there is
    /// more than one; else 0. The row updates in place as the run grows
    /// (1184).
    pub updates: usize,
    /// When the run's first report came.
    pub since: Option<WallTime>,
    /// The newest report's row only: whether the peer's position is alive
    /// (1209), which the row says instead of a count.
    pub liveness: Option<Liveness>,
    /// The photo it carries (1220): the bubble draws its thumbnail, a tap
    /// shows it whole.
    pub photo: Option<super::photos::PhotoRef>,
    /// The files it carries (1221), in the sender's order: the bubble
    /// draws a row for each, a tap saves and opens it.
    pub files: Vec<super::files::FileRef>,
    /// The voice message it carries (1222): the bubble draws its play
    /// button, length and level bar.
    pub voice: Option<super::voice::VoiceRef>,
    /// In a group's chat (1307), a copy's writer and the name the bubble
    /// carries above it; its body is then the text without the home's
    /// `"Name: "` prefix.
    pub writer: Option<([u8; 16], String)>,
    /// The invitation it carries (`lev 0x04`, 1307): the bubble is a card.
    pub invite: Option<[u8; 16]>,
    /// A group's line of its own (1307 G2): the body says who joined, left
    /// or was added ([`super::groups::membership_note`]); a screen draws
    /// it centred, as no one's bubble.
    pub note: bool,
    /// A group's line that came after newer ones (1307 pass four, study
    /// 6.1): a screen says "arrived late" under it, so nobody misses it.
    pub late: bool,
    /// How far its bytes have gone while they go (1662): ours only while
    /// it still sends, one being fetched until it is whole. Boxed: a
    /// bubble without one, nearly every bubble, pays a pointer (the M9's
    /// heap census, `m9_scene_census`).
    pub transfer: Option<alloc::boxed::Box<super::transfer::Transfer>>,
}

/// The bubbles of conversation `c`, oldest first: a reaction is a chip on
/// the bubble it names, not a bubble, and a run of reports is one bubble,
/// its newest, as the terminal folds it ([`super::Conversation::run`]).
/// In a relayed chat a reaction whose target is not on this device is a
/// bubble that says so (1482), never a chip on nothing: the target never
/// comes, a relay's copies carry their own ids. In a relayed chat
/// ([`super::relayed::is_relayed`]) each post is drawn under its writer,
/// without the relay's prefix.
pub fn bubbles<B>(model: &UiModel<B>, c: usize) -> Vec<Bubble> {
    let to = model.conversations.get(c).map_or(0, |c| c.messages.len());
    bubbles_in(model, c, 0, to)
}

/// The bubbles of messages `from..to` alone (1490): what the scene's
/// chat window builds. A reaction chip finds its target wherever the
/// reaction lies; a group line's dedupe sees the window only, so a
/// digest twin older than the window can show again beside it.
pub fn bubbles_in<B>(model: &UiModel<B>, c: usize, from: usize, to: usize) -> Vec<Bubble> {
    let bubbles = bubbles_of(model, c, from, to);
    model.count(super::counters::Op::BubblesBuilt, bubbles.len());
    bubbles
}

/// [`bubbles_in`], uncounted.
fn bubbles_of<B>(model: &UiModel<B>, c: usize, from: usize, to: usize) -> Vec<Bubble> {
    let Some(conversation) = model.conversations.get(c) else {
        return Vec::new();
    };
    let chips = reactions::gather(conversation);
    let newest_report = conversation.newest_fix_report();
    let liveness = newest_report.map(|_| model.liveness(&conversation.peer));
    let info = model.group(&conversation.peer);
    let group = info.is_some();
    let relayed = !group && super::relayed::is_relayed(conversation);
    // A reaction to a message this device does not hold (1482).
    // Only in a relay's chat: elsewhere a reaction that came before its
    // target waits for it, drawn nowhere until it arrives.
    let orphan = |message: &Message| {
        relayed
            .then(|| super::relayed::orphan_reaction(conversation, message))
            .flatten()
    };
    // A group's lines by their writer and the writer's clock (study 6.2):
    // one already shown, alone or in a digest, is not shown again, and one
    // older than a line shown before it arrived late (6.1).
    let mut seen: Vec<([u8; 16], u64)> = Vec::new();
    let mut newest_ms = 0u64;
    let mut once = move |sender: [u8; 16], sent_ms: u64| -> Option<bool> {
        if seen.contains(&(sender, sent_ms)) {
            return None;
        }
        seen.push((sender, sent_ms));
        let late = sent_ms < newest_ms;
        newest_ms = newest_ms.max(sent_ms);
        Some(late)
    };
    let writer_name = |address: &[u8; 16], name: &str| match model.names.shown(address) {
        Some(shown) => shown.to_string(),
        None if !name.is_empty() => name.to_string(),
        None => info
            .and_then(|g| g.member(address))
            .filter(|m| !m.name.is_empty())
            .map_or_else(
                || model.names.label(address, model.lang),
                |m| m.name.clone(),
            ),
    };
    let to = to.min(conversation.messages.len());
    let from = from.min(to);
    conversation.messages[from..to]
        .iter()
        .enumerate()
        .map(|(m, message)| (m + from, message))
        .map(|(m, message)| {
            let note = group
                .then(|| {
                    super::groups::membership_note(
                        &conversation.messages,
                        m,
                        model.address,
                        model.lang,
                    )
                })
                .flatten();
            (m, message, note)
        })
        .filter(|(m, message, note)| {
            (!message.is_aside() || note.is_some() || orphan(message).is_some())
                && !conversation.report_folded(*m)
        })
        .flat_map(|(m, message, note)| {
            let fix = model.message_fix(c, m);
            let run = conversation.run(m);
            let (updates, since) = match run.len() {
                1 => (0, None),
                n => (n, run.first().and_then(|&f| conversation.messages[f].at)),
            };
            let copy = super::groups::copy_of(&message.body, &message.fields, info)
                .filter(|_| group && !message.outbound);
            // The reader's own word for the writer, else the admin's
            // (1307); the bare name, as a group's sender line has it.
            let relay = relayed
                .then(|| super::relayed::writer_of(message, &conversation.peer))
                .flatten()
                .filter(|_| !message.outbound);
            let writer = match &relay {
                Some(w) => Some((w.address, model.relayed_name(w))),
                None => copy.map(|(address, name, _)| (address, writer_name(&address, name))),
            };
            let elsewhere = orphan(message).map(|content| {
                Text::Key(StrId::ReactedElsewhere, Vec::from([Arg::Str(content)]))
                    .resolve(model.lang)
            });
            let lev = (group && !message.outbound)
                .then(|| leviculum_lxmf::lev::Lev::from_fields(&message.fields))
                .flatten();
            let late = match lev.as_ref().and_then(|lev| lev.group_copy) {
                Some(pair) => match once(pair.sender, pair.sent_ms) {
                    Some(late) => late,
                    // Shown before: in a digest, or the same line again.
                    None => return Vec::new(),
                },
                None => false,
            };
            let digest = lev.and_then(|lev| lev.digest);
            let invite = leviculum_lxmf::lev::Lev::from_fields(&message.fields)
                .and_then(|lev| lev.invite)
                .map(|invite| invite.group)
                .filter(|group| model.group(group).is_some());
            let bubble = Bubble {
                key: message.activity,
                id: message.id,
                ours: message.outbound,
                note: note.is_some(),
                late,
                body: match (message.is_report(), &fix, copy) {
                    _ if note.is_some() => note.clone().unwrap_or_default(),
                    _ if elsewhere.is_some() => elsewhere.clone().unwrap_or_default(),
                    _ if relay.is_some() => relay.as_ref().map_or("", |w| w.text).to_string(),
                    (true, Some(_), _) => String::new(),
                    (true, None, _) => "position report".to_string(),
                    (false, _, Some((_, _, text))) => text.to_string(),
                    (false, _, None) => message.body.clone(),
                },
                writer,
                invite,
                mark: message
                    .outbound
                    .then(|| Mark::of(shown(message.delivery, model))),
                read_at: match shown(message.delivery, model) {
                    Some(DeliveryState::Read { at }) if message.outbound && at > 0 => Some(at),
                    _ => None,
                },
                witness: match shown(message.delivery, model) {
                    Some(DeliveryState::Delivered) => {
                        super::arrival::witness(message).map(|(witness, _)| witness)
                    }
                    _ => None,
                },
                at: message.at,
                chips: message
                    .id
                    .and_then(|id| chips.get(&id))
                    .map(|chips| {
                        chips
                            .iter()
                            .map(|chip| Chip {
                                content: chip.content.clone(),
                                count: chip.count,
                                ours: chip.own,
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                fix,
                quote: model.quoted(c, m),
                updates,
                since,
                liveness: liveness.filter(|_| newest_report == Some(m)),
                // A picture sent as a file shows as a photo does (1662),
                // its file's line under it.
                photo: super::photos::picture_of(&message.fields),
                files: super::files::files_of(&message.fields),
                voice: super::voice::voice_of(&message.fields),
                transfer: model
                    .transfers
                    .of(&conversation.peer, message.activity)
                    .filter(|_| {
                        !message.outbound
                            || Mark::of(shown(message.delivery, model)) == Mark::Sending
                    })
                    .map(alloc::boxed::Box::new),
            };
            // A digest (1307 pass four): one bubble a line, under its
            // writer's name and at its writer's time, as if each had come
            // alone; a stock member keeps the text.
            let Some(lines) = digest else {
                return Vec::from([bubble]);
            };
            let offset = message.at.map_or(0, |at| at.offset_secs);
            lines
                .into_iter()
                .filter_map(|line| {
                    let late = once(line.sender, line.sent_ms)?;
                    Some(Bubble {
                        body: super::groups::digest_line(&line, model.lang),
                        writer: Some((line.sender, writer_name(&line.sender, ""))),
                        at: Some(WallTime {
                            unix_secs: i64::try_from(line.sent_ms / 1000).unwrap_or(i64::MAX),
                            offset_secs: offset,
                        }),
                        late,
                        ..bubble.clone()
                    })
                })
                .collect()
        })
        .collect()
}

/// The open conversation as a pixel screen draws it: the top bar's peer,
/// name and avatar, the bubbles and where each sits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Open {
    pub peer: [u8; 16],
    pub name: String,
    pub initials: String,
    pub icon: Option<(String, String)>,
    pub bubbles: Vec<Bubble>,
    pub placements: Vec<Placement>,
    /// Whether the peer's position is alive (1209): the bar avatar's ring.
    pub liveness: Liveness,
}

/// The conversation the model has open, if any.
pub fn open<B>(model: &UiModel<B>) -> Option<Open> {
    let c = model.selected;
    let conversation = model.conversations.get(c)?;
    let peer = conversation.peer;
    let bubbles = bubbles(model, c);
    Some(Open {
        peer,
        name: model.names.label(&peer, model.lang),
        initials: initials(model.names.shown(&peer), &peer),
        icon: latest_appearance(&conversation.messages),
        placements: placements(&bubbles),
        bubbles,
        liveness: model.liveness(&peer),
    })
}

/// Whether `b` continues the group of `a`, the bubble before it: one
/// sender, one day, at most [`GROUP_SECS`] apart.
pub fn joins(a: &Bubble, b: &Bubble) -> bool {
    match (a.at, b.at) {
        (Some(at), Some(bt)) => {
            a.ours == b.ours
                && !a.note
                && !b.note
                && a.writer.as_ref().map(|w| w.0) == b.writer.as_ref().map(|w| w.0)
                && (bt.unix_secs - at.unix_secs).abs() <= GROUP_SECS
                && at.day() == bt.day()
        }
        _ => false,
    }
}

/// How one bubble sits among its neighbours.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Placement {
    /// It continues the group above: the gap is small, the inner top
    /// corner tight.
    pub joins_before: bool,
    /// The next continues its group: the inner bottom corner tight.
    pub joins_after: bool,
    /// A new day begins with it: a date line above.
    pub day: bool,
    /// It shows its time and mark: a group's last, or one of ours whose
    /// mark differs from the next one's.
    pub meta: bool,
}

/// Where each of `bubbles` sits.
pub fn placements(bubbles: &[Bubble]) -> Vec<Placement> {
    (0..bubbles.len())
        .map(|i| {
            let joins_before = i > 0 && joins(&bubbles[i - 1], &bubbles[i]);
            let next = bubbles.get(i + 1);
            let joins_after = next.is_some_and(|n| joins(&bubbles[i], n));
            let day = match (i.checked_sub(1).map(|j| &bubbles[j]), bubbles[i].at) {
                (_, None) => false,
                (None, Some(_)) => true,
                (Some(before), Some(at)) => before.at.is_none_or(|b| b.day() != at.day()),
            };
            let meta =
                !joins_after || bubbles[i].ours && next.is_some_and(|n| n.mark != bubbles[i].mark);
            Placement {
                joins_before,
                joins_after,
                day,
                meta,
            }
        })
        .collect()
}

/// The colours of a pixel screen, `0xaarrggbb`, by the names of the
/// phone's `colors.xml` (loquax-ui.md, "Pixel screens"). Every pair of
/// text and its ground keeps 4.5:1 or more, a mark or a control 3:1. The
/// chrome's roles are the suite's ([`leviculum_ui::pixel`]), which
/// lnomad's window draws in too; the bubbles and marks are Loquax's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Colours {
    pub background: u32,
    pub text: u32,
    pub text_secondary: u32,
    pub accent: u32,
    pub bar: u32,
    pub bubble_ours: u32,
    pub bubble_ours_text: u32,
    pub bubble_ours_meta: u32,
    pub bubble_theirs: u32,
    pub bubble_theirs_text: u32,
    pub bubble_theirs_meta: u32,
    pub failed_ground: u32,
    pub failed_mark: u32,
    /// Read's two ticks on our bubble (1232): the accent's family, bright
    /// enough for 3:1 on the bubble's green, where the accent itself is
    /// not. In the list read is the accent.
    pub read_mark: u32,
    pub send: u32,
    pub on_send: u32,
    pub badge: u32,
    pub badge_text: u32,
    pub list_failed_ground: u32,
    pub list_failed_mark: u32,
    pub dim: u32,
    pub panel: u32,
    pub failed_mark_on_panel: u32,
    pub reaction_ours: u32,
    pub selected_row: u32,
    /// The status dot on the bar's avatar (1199): the mailbox link up,
    /// and down. Marks, so 3:1 on the background is their floor.
    pub status_up: u32,
    pub status_down: u32,
    /// The recording bar (1276): its dot, timer and armed cancel hint,
    /// and the timer in the last 30 s before the cap. Text on the bar,
    /// so 4.5:1 on the background is their floor.
    pub rec_red: u32,
    pub rec_amber: u32,
    /// A field's placeholder (1448, walk finding 4): lighter than any
    /// written text, so an empty field never reads as filled.
    pub hint: u32,
    /// The own position on a map (1448, walk findings 8 and 16): blue, as
    /// the maps people know draw it. A mark, so 3:1 is its floor.
    pub own_dot: u32,
}

impl Colours {
    /// Each colour by its name, in the table's order.
    pub fn named(&self) -> [(&'static str, u32); 31] {
        [
            ("background", self.background),
            ("text", self.text),
            ("text_secondary", self.text_secondary),
            ("accent", self.accent),
            ("bar", self.bar),
            ("bubble_ours", self.bubble_ours),
            ("bubble_ours_text", self.bubble_ours_text),
            ("bubble_ours_meta", self.bubble_ours_meta),
            ("bubble_theirs", self.bubble_theirs),
            ("bubble_theirs_text", self.bubble_theirs_text),
            ("bubble_theirs_meta", self.bubble_theirs_meta),
            ("failed_ground", self.failed_ground),
            ("failed_mark", self.failed_mark),
            ("read_mark", self.read_mark),
            ("send", self.send),
            ("on_send", self.on_send),
            ("badge", self.badge),
            ("badge_text", self.badge_text),
            ("list_failed_ground", self.list_failed_ground),
            ("list_failed_mark", self.list_failed_mark),
            ("dim", self.dim),
            ("panel", self.panel),
            ("failed_mark_on_panel", self.failed_mark_on_panel),
            ("reaction_ours", self.reaction_ours),
            ("selected_row", self.selected_row),
            ("status_up", self.status_up),
            ("status_down", self.status_down),
            ("rec_red", self.rec_red),
            ("rec_amber", self.rec_amber),
            ("hint", self.hint),
            ("own_dot", self.own_dot),
        ]
    }
}

/// The light theme.
pub const LIGHT: Colours = Colours {
    background: pixel::LIGHT.background,
    text: pixel::LIGHT.text,
    text_secondary: pixel::LIGHT.text_secondary,
    accent: pixel::LIGHT.accent,
    bar: pixel::LIGHT.bar,
    bubble_ours: 0xff2e6b38,
    bubble_ours_text: 0xffffffff,
    bubble_ours_meta: 0xffd6eadb,
    bubble_theirs: 0xffe9eaed,
    bubble_theirs_text: 0xff1b1b1d,
    bubble_theirs_meta: 0xff55585e,
    failed_ground: 0xffffdad6,
    failed_mark: 0xff93000a,
    read_mark: 0xffb2ff59,
    send: 0xff2e6b38,
    on_send: 0xffffffff,
    badge: pixel::LIGHT.badge,
    badge_text: pixel::LIGHT.badge_text,
    list_failed_ground: 0xff93000a,
    list_failed_mark: 0xffffffff,
    dim: 0x99000000,
    panel: pixel::LIGHT.panel,
    failed_mark_on_panel: pixel::LIGHT.failed_on_panel,
    reaction_ours: 0xff5f9a69,
    selected_row: pixel::LIGHT.selected_row,
    status_up: 0xff2e7d32,
    status_down: 0xff80848c,
    rec_red: 0xffc62828,
    rec_amber: 0xff9a5b00,
    hint: 0xff8b8e95,
    own_dot: 0xff1a73e8,
};

/// The dark theme.
pub const DARK: Colours = Colours {
    background: pixel::DARK.background,
    text: pixel::DARK.text,
    text_secondary: pixel::DARK.text_secondary,
    accent: pixel::DARK.accent,
    bar: pixel::DARK.bar,
    bubble_ours: 0xff3b6b45,
    bubble_ours_text: 0xffffffff,
    bubble_ours_meta: 0xffdbeadd,
    bubble_theirs: 0xff303134,
    bubble_theirs_text: 0xffececec,
    bubble_theirs_meta: 0xffa9abb0,
    failed_ground: 0xffffdad6,
    failed_mark: 0xff93000a,
    read_mark: 0xffb2ff59,
    send: 0xff4f8f5b,
    on_send: 0xffffffff,
    badge: pixel::DARK.badge,
    badge_text: pixel::DARK.badge_text,
    list_failed_ground: 0xffffdad6,
    list_failed_mark: 0xff93000a,
    dim: 0xb3000000,
    panel: pixel::DARK.panel,
    failed_mark_on_panel: pixel::DARK.failed_on_panel,
    reaction_ours: 0xff4f8f5b,
    selected_row: pixel::DARK.selected_row,
    status_up: 0xff81c784,
    status_down: 0xff7d8087,
    rec_red: 0xffee6464,
    rec_amber: 0xffffb74d,
    hint: 0xff7c7f86,
    own_dot: 0xff8ab4f8,
};

/// The sizes of a pixel screen, in the phone's dp and sp, which the
/// window takes as logical pixels (loquax-ui.md, "Pixel screens").
pub mod size {
    /// The list's row: its least height, its avatar, the gap after it.
    pub const ROW_MIN: f32 = 80.0;
    pub const ROW_AVATAR: f32 = 56.0;
    pub const ROW_GAP: f32 = 16.0;
    /// The list's name, last line, time and unread count.
    pub const ROW_NAME: f32 = 18.0;
    pub const ROW_LAST: f32 = 15.0;
    pub const ROW_TIME: f32 = 13.0;
    pub const BADGE_TEXT: f32 = 12.0;
    /// The unread count's pill: its height and its corner.
    pub const BADGE: f32 = 22.0;
    pub const BADGE_CORNER: f32 = 11.0;
    /// The chat's top bar, its avatar and its name.
    pub const BAR: f32 = 64.0;
    pub const BAR_AVATAR: f32 = 40.0;
    pub const BAR_NAME: f32 = 20.0;
    /// A bubble's body, its time and mark, its corner and a grouped
    /// bubble's inner one, and the part of the chat it may be wide.
    pub const BODY: f32 = 17.0;
    pub const META: f32 = 12.0;
    pub const CORNER: f32 = 18.0;
    pub const CORNER_GROUPED: f32 = 6.0;
    pub const BUBBLE_PART: f32 = 0.8;
    /// The gap above a bubble that starts a group, and one that joins.
    pub const GAP: f32 = 10.0;
    pub const GAP_GROUPED: f32 = 2.0;
    /// The date line between days.
    pub const DAY: f32 = 14.0;
    /// A reaction's pill: its text and its corner.
    pub const PILL: f32 = 14.0;
    pub const PILL_CORNER: f32 = 14.0;
    /// The compose bar: the pill's least height and its corner, its text,
    /// the round send button.
    pub const COMPOSE_MIN: f32 = 48.0;
    pub const COMPOSE_CORNER: f32 = 24.0;
    pub const COMPOSE_TEXT: f32 = 17.0;
    pub const SEND: f32 = 48.0;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::Signature;
    use alloc::vec;
    use leviculum_lxmf::constants::{FIELD_REACTION, FIELD_TELEMETRY};
    use leviculum_lxmf::reaction::Reaction;

    const PEER: [u8; 16] = [0x3f; 16];

    #[test]
    fn initials_are_first_and_last_word() {
        assert_eq!(initials(Some("Jörn Brunke"), &PEER), "JB");
        assert_eq!(initials(Some("anna maria schulz"), &PEER), "AS");
        assert_eq!(initials(Some("lew"), &PEER), "L");
        assert_eq!(initials(Some("  (Basis)  Nord "), &PEER), "BN");
        assert_eq!(initials(Some("🦊"), &PEER), "🦊");
        assert_eq!(initials(Some("ß"), &PEER), "SS");
    }

    #[test]
    fn a_peer_without_a_name_shows_no_hex() {
        // 1452: never the address; the avatar's colour tells two apart.
        assert_eq!(initials(None, &PEER), "?");
        assert_eq!(initials(Some("   "), &PEER), "?");
    }

    fn icon(name: &str, fg: &[u8], bg: &[u8]) -> Field {
        let mut value = Vec::new();
        msgpack::array(&mut value, 3);
        msgpack::string(&mut value, name);
        msgpack::bin(&mut value, fg);
        msgpack::bin(&mut value, bg);
        (FIELD_ICON_APPEARANCE, value)
    }

    #[test]
    fn an_icon_appearance_gives_its_colours() {
        let fields = vec![
            (0x02, vec![0xc0]),
            icon("account", &[255, 255, 255], &[0x1e, 0x88, 0xe5]),
        ];
        let got = appearance(&fields);
        assert_eq!(got, Some(("ffffff".to_string(), "1e88e5".to_string())));
        assert_eq!(avatar_colours(&PEER, got.as_ref()), (0xffffff, 0x1e88e5));
    }

    #[test]
    fn without_an_icon_the_address_picks_the_ground() {
        assert_eq!(avatar_colours(&PEER, None), (0xffffff, AVATAR_PALETTE[7]));
        let odd = ("fff".to_string(), "zzzzzz".to_string());
        assert_eq!(
            avatar_colours(&[0; 16], Some(&odd)),
            (0xffffff, AVATAR_PALETTE[0])
        );
    }

    #[test]
    fn a_malformed_icon_appearance_gives_none() {
        assert_eq!(appearance(&[]), None);
        assert_eq!(appearance(&[icon("x", &[1, 2], &[3, 4, 5])]), None);
        assert_eq!(
            appearance(&[(FIELD_ICON_APPEARANCE, vec![0x92, 0xa1, b'x', 0xc0])]),
            None
        );
        assert_eq!(
            appearance(&[(FIELD_ICON_APPEARANCE, vec![0xa1, b'x'])]),
            None
        );
        assert_eq!(appearance(&[(FIELD_ICON_APPEARANCE, Vec::new())]), None);
    }

    fn message(outbound: bool, body: &str, fields: Vec<Field>) -> Message {
        Message {
            outbound,
            body: body.to_string(),
            id: None,
            delivery: outbound.then_some(DeliveryState::Delivered),
            signature: Signature::Verified,
            leg: None,
            unproven: false,
            at: None,
            size: body.len(),
            ledger: Vec::new(),
            last_leg: None,
            activity: 0,
            fields,
        }
    }

    #[test]
    fn the_list_tells_a_text_a_position_and_a_reaction_apart() {
        assert_eq!(kind(&message(false, "hallo", Vec::new())), "text");
        let report = message(false, "", vec![(FIELD_TELEMETRY, vec![0xc0])]);
        assert_eq!(kind(&report), "position");
        // Text with telemetry is a text: the line shows what was written.
        let with_fix = message(false, "bin da", vec![(FIELD_TELEMETRY, vec![0xc0])]);
        assert_eq!(kind(&with_fix), "text");
        // A position we shared (1201) is one too, worded as ours.
        let shared = message(true, "", vec![(FIELD_TELEMETRY, vec![0xc0])]);
        assert_eq!(kind(&shared), "position");
        assert_eq!(preview(&shared, Lang::En), "Position shared");
        assert_eq!(preview(&shared, Lang::De), "Position geteilt");
        let thumbs = Reaction {
            to: [7; 32],
            content: "👍".to_string(),
        }
        .encode_field_value();
        let reaction = message(true, "", vec![(FIELD_REACTION, thumbs)]);
        assert_eq!(kind(&reaction), "reaction");
        assert_eq!(preview(&reaction, Lang::En), "You reacted 👍");
        assert_eq!(preview(&reaction, Lang::De), "Du hast mit 👍 reagiert");
    }

    fn bubble(ours: bool, at: Option<i64>, mark: Option<Mark>) -> Bubble {
        Bubble {
            key: 0,
            id: None,
            ours,
            body: String::new(),
            mark,
            read_at: None,
            witness: None,
            at: at.map(|unix_secs| WallTime {
                unix_secs,
                offset_secs: 3600,
            }),
            chips: Vec::new(),
            fix: None,
            quote: None,
            updates: 0,
            since: None,
            liveness: None,
            photo: None,
            files: Vec::new(),
            voice: None,
            writer: None,
            invite: None,
            note: false,
            late: false,
            transfer: None,
        }
    }

    /// The phone's grouping (1129): one sender within three minutes on one
    /// day is a group, whose last alone shows its time, except one of ours
    /// whose mark differs from the next one's; a new day draws a date line.
    #[test]
    fn bubbles_group_by_sender_minutes_and_day() {
        // 2026-10-30 10:00 local is 09:00 UTC.
        let t = 1_793_350_800;
        let delivered = Some(Mark::Delivered);
        let list = [
            bubble(false, Some(t), None),
            bubble(false, Some(t + 60), None),
            bubble(true, Some(t + 120), delivered),
            bubble(true, Some(t + 150), Some(Mark::Sending)),
            bubble(true, Some(t + 160), Some(Mark::Sending)),
            // Four minutes later: a group of its own.
            bubble(true, Some(t + 400), Some(Mark::Sending)),
            // The next day.
            bubble(true, Some(t + 86_400), Some(Mark::Sending)),
            bubble(false, None, None),
        ];
        let p = placements(&list);
        let joined: Vec<bool> = p.iter().map(|p| p.joins_before).collect();
        assert_eq!(
            joined,
            [false, true, false, true, true, false, false, false]
        );
        let meta: Vec<bool> = p.iter().map(|p| p.meta).collect();
        assert_eq!(meta, [false, true, true, false, true, true, true, true]);
        let day: Vec<bool> = p.iter().map(|p| p.day).collect();
        assert_eq!(day, [true, false, false, false, false, false, true, false]);
        assert!(p[0].joins_after && !p[1].joins_after);
    }

    #[test]
    fn the_search_finds_every_word_in_the_name_or_the_line() {
        let row = ListRow {
            peer: PEER,
            name: "Anna Schulz <3f3f3f>".to_string(),
            last: "Bis morgen am Tor".to_string(),
            ours: false,
            at: None,
            unread: 0,
            updates: 0,
            initials: "AS".to_string(),
            icon: None,
            kind: "text",
            reaction: None,
            mark: None,
            read_at: None,
            pinned: false,
            muted: false,
            liveness: Liveness::default(),
            draft: None,
        };
        assert!(row_matches(&row, ""));
        assert!(row_matches(&row, "anna"));
        assert!(row_matches(&row, "schulz tor"));
        assert!(!row_matches(&row, "anna bahnhof"));
    }

    #[cfg(feature = "std")]
    fn contrast(a: u32, b: u32) -> f64 {
        let lum = |c: u32| {
            let f = |v: u32| {
                let v = f64::from(v & 0xff) / 255.0;
                match v <= 0.03928 {
                    true => v / 12.92,
                    false => ((v + 0.055) / 1.055).powf(2.4),
                }
            };
            0.2126 * f(c >> 16) + 0.7152 * f(c >> 8) + 0.0722 * f(c)
        };
        let (x, y) = (lum(a), lum(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    /// Every mark keeps 3:1 on what it sits on, in both themes: our
    /// bubble in the chat, the background in the list, the failed sign on
    /// its own disc; and read is told from the grey ticks by more than a
    /// shade (1232).
    #[test]
    #[cfg(feature = "std")]
    fn every_mark_holds_three_to_one_on_its_ground() {
        let mut fails = Vec::new();
        for (theme, c) in [("light", LIGHT), ("dark", DARK)] {
            let mut check = |what: String, ink: u32, ground: u32| {
                let ratio = contrast(ink, ground);
                if ratio < 3.0 {
                    fails.push(format!("{theme} {what}: {ratio:.2}"));
                }
            };
            for mark in MARKS {
                let (bubble, list) = match mark {
                    Mark::Failed => (c.failed_ground, c.list_failed_ground),
                    _ => (bubble_ink(mark, &c), list_ink(mark, &c)),
                };
                check(format!("{mark:?} on our bubble"), bubble, c.bubble_ours);
                check(format!("{mark:?} in the list"), list, c.background);
            }
            check("the failed sign".into(), c.failed_mark, c.failed_ground);
            check(
                "the list's failed sign".into(),
                c.list_failed_mark,
                c.list_failed_ground,
            );
            // The read tick against both grounds it is drawn on.
            check("read on our bubble".into(), c.read_mark, c.bubble_ours);
            check("read in the list".into(), c.accent, c.background);
            assert_ne!(c.read_mark, c.bubble_ours_meta, "{theme}");
            assert_ne!(c.accent, c.text_secondary, "{theme}");
        }
        assert!(fails.is_empty(), "{fails:#?}");
    }

    /// The shortest distance from point `p` to the segment `a`-`b`.
    #[cfg(feature = "std")]
    fn to_segment(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
        let (x, y) = (a.0 + t * dx - p.0, a.1 + t * dy - p.1);
        (x * x + y * y).sqrt()
    }

    /// The two ticks read as two (loquax-read-receipts.md, 3.4): the second
    /// is the first moved 6.5 units right, and no point of the second's
    /// stroke comes nearer the first's than a clear gap, at read's heavier
    /// stroke too; everything stays on the grid.
    #[test]
    #[cfg(feature = "std")]
    fn the_two_ticks_never_touch() {
        let [a, b] = TWO_TICKS else {
            panic!("two ticks");
        };
        assert_eq!((b[2].0 - a[2].0, b[2].1 - a[2].1), (6.5, 0.0));
        assert_eq!((b[1].0 - a[1].0, b[1].1 - a[1].1), (6.5, 0.0));
        for shape in [TICKS, TICKS_READ] {
            let mut nearest = f32::MAX;
            for (from, to) in [(b, a), (a, b)] {
                for pair in from.windows(2) {
                    for step in 0..=100 {
                        let t = step as f32 / 100.0;
                        let p = (
                            pair[0].0 + t * (pair[1].0 - pair[0].0),
                            pair[0].1 + t * (pair[1].1 - pair[0].1),
                        );
                        for leg in to.windows(2) {
                            nearest = nearest.min(to_segment(p, leg[0], leg[1]));
                        }
                    }
                }
            }
            let gap = nearest - shape.width;
            assert!(gap >= 0.7, "width {}: a gap of {gap:.2} units", shape.width);
        }
        for mark in MARKS {
            let shape = mark.shape();
            let half = shape.width / 2.0;
            for &(x, y) in shape.lines.iter().flat_map(|l| l.iter()) {
                assert!(x - half >= 0.0 && x + half <= GRID, "{mark:?} x {x}");
                assert!(y - half >= 0.0 && y + half <= GRID, "{mark:?} y {y}");
            }
        }
        const { assert!(TICKS_READ.width > TICKS.width) };
        assert_eq!(TICKS_READ.lines, TICKS.lines);
    }

    /// Each state of the core's ten lands on its rung of seven.
    #[test]
    fn each_delivery_state_has_its_rung() {
        use DeliveryState as S;
        let rungs: Vec<(Option<DeliveryState>, Mark)> = [
            None,
            Some(S::Queued),
            Some(S::Stamping),
            Some(S::InFlight),
            Some(S::Sent),
            Some(S::InMailbox),
            Some(S::Arrived),
            Some(S::Delivered),
            Some(S::Read { at: 7 }),
            Some(S::Rejected),
            Some(S::Failed),
        ]
        .into_iter()
        .map(|state| (state, Mark::of(state)))
        .collect();
        let want = [
            Mark::Sending,
            Mark::Sending,
            Mark::Sending,
            Mark::Sending,
            Mark::Unconfirmed,
            Mark::Mailbox,
            Mark::Arrived,
            Mark::Delivered,
            Mark::Read,
            Mark::Failed,
            Mark::Failed,
        ];
        assert_eq!(rungs.iter().map(|(_, m)| *m).collect::<Vec<_>>(), want);
        let words: Vec<&str> = MARKS.iter().map(|m| m.word()).collect();
        assert_eq!(
            words,
            [
                "sending",
                "unconfirmed",
                "mailbox",
                "arrived",
                "delivered",
                "read",
                "failed"
            ]
        );
        let resources: Vec<&str> = MARKS.iter().map(|m| m.resource()).collect();
        assert_eq!(
            resources,
            [
                "state_clock",
                "state_clock",
                "state_tick",
                "state_tick",
                "state_ticks",
                "state_ticks_read",
                "state_failed"
            ]
        );
    }

    /// The chrome of both themes is the suite's, the colours lnomad's
    /// window draws in ([`leviculum_ui::pixel`]).
    #[test]
    fn the_chrome_is_the_suites() {
        for (ours, suite) in [(LIGHT, pixel::LIGHT), (DARK, pixel::DARK)] {
            assert_eq!(
                [
                    ours.background,
                    ours.text,
                    ours.text_secondary,
                    ours.accent,
                    ours.bar,
                    ours.panel,
                    ours.failed_mark_on_panel,
                    ours.selected_row,
                    ours.badge,
                    ours.badge_text,
                ],
                [
                    suite.background,
                    suite.text,
                    suite.text_secondary,
                    suite.accent,
                    suite.bar,
                    suite.panel,
                    suite.failed_on_panel,
                    suite.selected_row,
                    suite.badge,
                    suite.badge_text,
                ]
            );
        }
    }

    /// The phone's `colors.xml` holds the same table, name for name, so
    /// the two screens cannot drift (loquax-ui.md, "Pixel screens").
    #[test]
    #[cfg(feature = "std")]
    fn the_phone_draws_the_same_colours() {
        for (dir, colours) in [("values", LIGHT), ("values-night", DARK)] {
            let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../loquax-android/android/res")
                .join(dir)
                .join("colors.xml");
            let xml = std::fs::read_to_string(&path).expect("colors.xml");
            let mut seen = Vec::new();
            for line in xml.lines() {
                let line = line.trim();
                let Some(rest) = line.strip_prefix("<color name=\"") else {
                    continue;
                };
                let (name, rest) = rest.split_once("\">#").expect("a colour");
                let hex = rest.split_once('<').expect("its end").0;
                let value = u32::from_str_radix(hex, 16).expect("hex");
                let value = match hex.len() {
                    6 => 0xff00_0000 | value,
                    _ => value,
                };
                seen.push((name.to_string(), value));
            }
            let want: Vec<(String, u32)> = colours
                .named()
                .iter()
                .map(|(n, v)| (n.to_string(), *v))
                .collect();
            assert_eq!(seen, want, "{}", path.display());
        }
    }
}
