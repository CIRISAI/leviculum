//! The chat as a pixel screen draws it (1145): the conversation list's rows
//! and the open conversation's bubbles, grouped and dated, the avatars'
//! letters and colours, and the colour and size tokens of
//! `docs/src/concepts/loquax-ui.md`, section "Pixel screens". The phone's
//! facade (`loquax-android`) and the desktop window (`loquax gui`) both
//! draw from here, so a bubble, a mark or a group means the same on each.
//!
//! The terminal draws the same model through [`super::layout`]; it has
//! cells, not bubbles, and reads none of this.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use leviculum_lxmf::constants::FIELD_ICON_APPEARANCE;
use leviculum_lxmf::msgpack;
use leviculum_lxmf::Field;
use leviculum_ui::delivery::DeliveryState;

use super::reactions;
use super::replies::Quoted;
use super::{Message, UiModel};
use crate::telemetry::{run_words, Fix};
use crate::when::WallTime;

/// Messages of one sender this close together form a group.
pub const GROUP_SECS: i64 = 180;

/// The five marks a pixel screen draws, from the glyph table's nine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Queued,
    Sending,
    Sent,
    Delivered,
    Failed,
}

impl Mark {
    pub fn of(state: Option<DeliveryState>) -> Self {
        match state {
            None | Some(DeliveryState::Queued) | Some(DeliveryState::Stamping) => Self::Queued,
            Some(DeliveryState::InFlight) => Self::Sending,
            Some(DeliveryState::Sent) | Some(DeliveryState::InMailbox) => Self::Sent,
            Some(DeliveryState::Delivered) => Self::Delivered,
            Some(DeliveryState::Rejected) | Some(DeliveryState::Failed) => Self::Failed,
        }
    }

    /// The facade's word: `queued`, `sending`, `sent`, `delivered`,
    /// `failed`.
    pub fn word(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Sending => "sending",
            Self::Sent => "sent",
            Self::Delivered => "delivered",
            Self::Failed => "failed",
        }
    }
}

/// The initials for a peer (1129): the first letter of the name's first
/// word and of its last, one letter for a name of one word, and the
/// address's first two hex digits for a peer without a name.
pub fn initials(name: Option<&str>, peer: &[u8; 16]) -> String {
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
        return crate::address::to_hex(&peer[..1]).to_uppercase();
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
    } else {
        "text"
    }
}

/// The list's line for a conversation's last message.
pub fn preview(message: &Message) -> String {
    if let Some(text) = reactions::preview(message) {
        return text;
    }
    if message.is_report() {
        return "position report".to_string();
    }
    if message.is_position() {
        return "position shared".to_string();
    }
    message.body.lines().next().unwrap_or_default().to_string()
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
    /// Unread rows: a run of reports counts once (1184).
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
    pub pinned: bool,
    pub muted: bool,
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
    let conversation = model.conversations.iter().find(|c| c.peer == *peer);
    let messages = conversation.map_or(&[][..], |c| c.messages.as_slice());
    let last = messages.last();
    // A run of reports is one line with its count, not the last of many.
    let updates = conversation
        .filter(|_| last.is_some_and(Message::is_report))
        .map_or(0, |c| c.run(messages.len().saturating_sub(1)).len());
    ListRow {
        peer: *peer,
        name: model.names.label(peer),
        last: last
            .map(|m| preview(m) + &run_words(updates, None))
            .unwrap_or_default(),
        ours: last.is_some_and(|m| m.outbound),
        at: last.and_then(|m| m.at),
        unread: conversation.map_or(0, |c| c.unread_rows()),
        updates,
        initials: initials(model.names.shown(peer), peer),
        icon: latest_appearance(messages),
        kind: last.map_or("", kind),
        reaction: last
            .filter(|m| m.is_reaction())
            .and_then(|m| m.reaction())
            .map(|(_, content)| content),
        mark: last.filter(|m| m.outbound).map(|m| Mark::of(m.delivery)),
        pinned: conversation.is_some_and(|c| c.pinned),
        muted: model.muted(peer),
    }
}

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
}

/// The bubbles of conversation `c`, oldest first: a reaction is a chip on
/// the bubble it names, not a bubble, and a run of reports is one bubble,
/// its newest, as the terminal folds it ([`super::Conversation::run`]).
pub fn bubbles<B>(model: &UiModel<B>, c: usize) -> Vec<Bubble> {
    let Some(conversation) = model.conversations.get(c) else {
        return Vec::new();
    };
    let chips = reactions::gather(conversation);
    conversation
        .messages
        .iter()
        .enumerate()
        .filter(|(m, message)| !message.is_reaction() && !conversation.report_folded(*m))
        .map(|(m, message)| {
            let fix = model.message_fix(c, m);
            let run = conversation.run(m);
            let (updates, since) = match run.len() {
                1 => (0, None),
                n => (n, run.first().and_then(|&f| conversation.messages[f].at)),
            };
            Bubble {
                key: message.activity,
                id: message.id,
                ours: message.outbound,
                body: match (message.is_report(), &fix) {
                    (true, Some(_)) => String::new(),
                    (true, None) => "position report".to_string(),
                    (false, _) => message.body.clone(),
                },
                mark: message.outbound.then(|| Mark::of(message.delivery)),
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
            }
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
}

/// The conversation the model has open, if any.
pub fn open<B>(model: &UiModel<B>) -> Option<Open> {
    let c = model.selected;
    let conversation = model.conversations.get(c)?;
    let peer = conversation.peer;
    let bubbles = bubbles(model, c);
    Some(Open {
        peer,
        name: model.names.label(&peer),
        initials: initials(model.names.shown(&peer), &peer),
        icon: latest_appearance(&conversation.messages),
        placements: placements(&bubbles),
        bubbles,
    })
}

/// Whether `b` continues the group of `a`, the bubble before it: one
/// sender, one day, at most [`GROUP_SECS`] apart.
pub fn joins(a: &Bubble, b: &Bubble) -> bool {
    match (a.at, b.at) {
        (Some(at), Some(bt)) => {
            a.ours == b.ours
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
/// text and its ground keeps 4.5:1 or more, a mark or a control 3:1.
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
}

impl Colours {
    /// Each colour by its name, in the table's order.
    pub fn named(&self) -> [(&'static str, u32); 26] {
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
        ]
    }
}

/// The light theme.
pub const LIGHT: Colours = Colours {
    background: 0xffffffff,
    text: 0xff1b1b1d,
    text_secondary: 0xff5c5f66,
    accent: 0xff3a7d44,
    bar: 0xfff1f2f4,
    bubble_ours: 0xff2e6b38,
    bubble_ours_text: 0xffffffff,
    bubble_ours_meta: 0xffd6eadb,
    bubble_theirs: 0xffe9eaed,
    bubble_theirs_text: 0xff1b1b1d,
    bubble_theirs_meta: 0xff55585e,
    failed_ground: 0xffffdad6,
    failed_mark: 0xff93000a,
    send: 0xff2e6b38,
    on_send: 0xffffffff,
    badge: 0xff2e6b38,
    badge_text: 0xffffffff,
    list_failed_ground: 0xff93000a,
    list_failed_mark: 0xffffffff,
    dim: 0x99000000,
    panel: 0xffffffff,
    failed_mark_on_panel: 0xffba1a1a,
    reaction_ours: 0xff5f9a69,
    selected_row: 0x403a7d44,
    status_up: 0xff2e7d32,
    status_down: 0xff80848c,
};

/// The dark theme.
pub const DARK: Colours = Colours {
    background: 0xff17181b,
    text: 0xffececec,
    text_secondary: 0xffa9abb0,
    accent: 0xff81c784,
    bar: 0xff26272b,
    bubble_ours: 0xff3b6b45,
    bubble_ours_text: 0xffffffff,
    bubble_ours_meta: 0xffdbeadd,
    bubble_theirs: 0xff303134,
    bubble_theirs_text: 0xffececec,
    bubble_theirs_meta: 0xffa9abb0,
    failed_ground: 0xffffdad6,
    failed_mark: 0xff93000a,
    send: 0xff4f8f5b,
    on_send: 0xffffffff,
    badge: 0xff3b6b45,
    badge_text: 0xffffffff,
    list_failed_ground: 0xffffdad6,
    list_failed_mark: 0xff93000a,
    dim: 0xb3000000,
    panel: 0xff2c2d31,
    failed_mark_on_panel: 0xffffb4ab,
    reaction_ours: 0xff4f8f5b,
    selected_row: 0x4081c784,
    status_up: 0xff81c784,
    status_down: 0xff7d8087,
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
    fn a_peer_without_a_name_shows_its_address() {
        assert_eq!(initials(None, &PEER), "3F");
        assert_eq!(initials(Some("   "), &PEER), "3F");
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
        assert_eq!(preview(&shared), "position shared");
        let thumbs = Reaction {
            to: [7; 32],
            content: "👍".to_string(),
        }
        .encode_field_value();
        let reaction = message(true, "", vec![(FIELD_REACTION, thumbs)]);
        assert_eq!(kind(&reaction), "reaction");
        assert_eq!(preview(&reaction), "you reacted 👍");
    }

    fn bubble(ours: bool, at: Option<i64>, mark: Option<Mark>) -> Bubble {
        Bubble {
            key: 0,
            id: None,
            ours,
            body: String::new(),
            mark,
            at: at.map(|unix_secs| WallTime {
                unix_secs,
                offset_secs: 3600,
            }),
            chips: Vec::new(),
            fix: None,
            quote: None,
            updates: 0,
            since: None,
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
            pinned: false,
            muted: false,
        };
        assert!(row_matches(&row, ""));
        assert!(row_matches(&row, "anna"));
        assert!(row_matches(&row, "schulz tor"));
        assert!(!row_matches(&row, "anna bahnhof"));
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
