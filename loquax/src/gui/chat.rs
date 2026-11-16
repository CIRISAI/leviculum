//! The window's chat (1145), after the phone's design: the conversation
//! list on the left, avatars with initials in the peer's icon colours, the
//! name, the last line, the list's time, an unread count or our delivery
//! mark, a search and a new conversation; the open conversation on the
//! right, a top bar with the avatar and the name, rounded bubbles, ours
//! right in the accent, theirs left, grouped as the phone groups them
//! ([`loquax::ui::chat::placements`]), date lines between days, reaction
//! pills, position lines that open the map, and a pill compose bar with an
//! emoji button and a round send button.
//!
//! Everything said here comes from the scene ([`loquax::ui::scene`],
//! 1217/1218), what the phone's facade is derived from too: which rows in
//! which order, the grouping, the day lines, the liveness words and rings,
//! what a click on a node does and what a bubble's menu offers; the
//! colours and sizes are [`loquax::ui::chat`]'s tokens. This file only
//! decides how iced draws the nodes, one function per node type, and every
//! press it makes is the node's [`Intent`].
//!
//! The window shows the shared structure (1218): the reader's top bar with
//! the avatar, the bar's actions as drawn icons, the tabs with the unread
//! count. In the Wide class the tabs are a rail at the left, the list and
//! the chat beside each other; in the Compact class (a narrow window) the
//! list with the tabs at the bottom, or the open chat alone with a back in
//! its bar, as the phone stacks them.

use iced::widget::{
    button, column, container, mouse_area, progress_bar, row, scrollable, text, text_editor,
    text_input, toggler, tooltip, Space,
};
use iced::{border, mouse, Alignment, Background, Border, Color, Element, Length, Theme};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::ops::Range;

use leviculum_ui_iced::chrome;
pub use leviculum_ui_iced::chrome::RAIL;
use leviculum_ui_iced::rows::{rows, Item};
use leviculum_ui_iced::select::selectable;
use leviculum_ui_iced::{avatar as face, control, drawing, Palette};
pub use leviculum_ui_iced::{colour, rgb, Icon};

use loquax::ui::chat::{self, size, Colours, Mark};
use loquax::ui::compose::Button as BarButton;
use loquax::ui::group_scene::InviteNode;
use loquax::ui::groups::{Step as GroupStep, Verb as GroupVerb};
use loquax::ui::intent::{Intent, Switch};
use loquax::ui::reactions::SIGNAL_QUICK;
use loquax::ui::recording::Verb;
use loquax::ui::scene::{
    self, Action, BubbleNode, Chat, LayoutClass, Lead, RecordingBar, Row, Tab, TabId,
};
use loquax::ui::selection::{peer_key, position_line, preview_line, Part, RunKey};
use loquax::ui::strings::{Arg, Lang, StrId, Text};
use loquax::when::WallTime;

use super::{App, Message};

/// The list's width, in logical pixels.
pub const LIST_WIDTH: f32 = 340.0;

/// A room's bubble's avatar, beside the first of another's run (1450).
const ROOM_FACE: f32 = 28.0;

/// The search field's id: the top bar's search focuses it.
pub const SEARCH_FIELD: &str = "search";
/// The Advanced section's field for another hub's address (1577), which a
/// press on "Another hub: paste its address" focuses.
pub const HUB_FIELD: &str = "rooms-hub";

/// The position sheet's field's id (1371): "Type my position" focuses it.
pub const POSITION_FIELD: &str = "position";

/// The tokens of the theme the window is in.
pub fn tokens(dark: bool) -> Colours {
    match dark {
        true => chat::DARK,
        false => chat::LIGHT,
    }
}

/// The roles the shared widgets paint with, from the tokens.
pub fn palette(t: Colours) -> Palette {
    Palette {
        background: colour(t.background),
        bar: colour(t.bar),
        panel: colour(t.panel),
        text: colour(t.text),
        text_secondary: colour(t.text_secondary),
        accent: colour(t.accent),
        selected_row: colour(t.selected_row),
        badge: colour(t.badge),
        badge_text: colour(t.badge_text),
    }
}

/// A delivery mark as the shared glyph icon.
/// A line of the chat's text, in pixels: a wheel's notch scrolls three
/// (1431).
pub const LINE: f32 = size::BODY * 1.3;

/// A line of the open chat (1487): what the column of the chat held as
/// one child, by the group and the bubble it belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChatLine {
    Day(usize),
    Named(usize),
    Note(usize, usize),
    Gap(usize, usize),
    Bubble(usize, usize),
    Late(usize, usize),
    Actions(usize, usize),
    Foot,
}

/// `value` hashed: a line's stamp.
fn stamp_of(value: impl Hash) -> u64 {
    let mut h = std::hash::DefaultHasher::new();
    value.hash(&mut h);
    h.finish()
}

/// What of a bubble decides its height (1487), hashed: its words, its
/// quote, what it carries, its position line, whether its time and mark
/// show, its reactions. Its colours, corners and selection do not move
/// it. The reader's language and the chat's width are the list's, mixed
/// into every stamp in [`App::chat_lines`].
fn bubble_stamp(node: &BubbleNode) -> u64 {
    let mut h = std::hash::DefaultHasher::new();
    let bubble = &node.bubble;
    bubble.ours.hash(&mut h);
    node.text.hash(&mut h);
    bubble
        .quote
        .as_ref()
        .map(|q| (q.ours, &q.text))
        .hash(&mut h);
    node.photo.as_ref().map(|p| p.file).hash(&mut h);
    node.progress.is_some().hash(&mut h);
    for file in &node.files {
        (&file.name, &file.size).hash(&mut h);
    }
    node.voice
        .as_ref()
        .map(|v| (v.levels.len(), v.toggle.is_some(), &v.duration))
        .hash(&mut h);
    // An invitation's card, a position's line and its live words are rare
    // enough to be hashed as they print.
    if let Some(invite) = &node.invite {
        format!("{invite:?}").hash(&mut h);
    }
    if bubble.fix.is_some() {
        format!("{:?}{:?}{:?}", bubble.fix, node.position, node.live).hash(&mut h);
    }
    (node.place.meta, bubble.at.is_some(), node.mark.is_some()).hash(&mut h);
    for chip in &bubble.chips {
        (&chip.content, chip.count).hash(&mut h);
    }
    h.finish()
}

/// What of a conversation's row decides its height (1487), hashed: the
/// lines it has under the name and what stands beside them. Its lines do
/// not wrap, so their words do not.
fn row_stamp(r: &Row) -> u64 {
    let c = &r.row;
    stamp_of((
        (
            c.unread > 0,
            c.mark.is_some(),
            c.pinned,
            c.muted,
            c.draft.is_some(),
        ),
        (r.live.is_some(), r.position.is_some(), r.draft.is_some()),
        (r.sharing, r.group.is_some(), r.same_name.is_some()),
    ))
}

/// A list whose wheel scrolls softly (1431): a notch three lines eased
/// over the frames that follow, a touchpad's pixels as they come
/// ([`leviculum_ui_iced::smooth`]).
pub fn soft<'a>(list: iced::widget::Scrollable<'a, Message>) -> Element<'a, Message> {
    leviculum_ui_iced::smooth::smooth(list, LINE).into()
}

/// [`soft`], and while `picking` (a selection's drag is on, 1475) the
/// pointer at its top or bottom edge scrolls it.
fn soft_picking<'a>(
    list: iced::widget::Scrollable<'a, Message>,
    picking: bool,
) -> Element<'a, Message> {
    leviculum_ui_iced::smooth::smooth(list, LINE)
        .edge_scroll(picking)
        .into()
}

/// The compose field's padding, top and left: where a middle click's
/// point lands in its text (1475).
pub const COMPOSE_PAD_Y: f32 = 6.0;
pub const COMPOSE_PAD_X: f32 = 4.0;

fn mark_icon(mark: Mark) -> Icon {
    Icon::Glyph {
        shape: mark.shape(),
        failed: mark == Mark::Failed,
    }
}

/// The reader's avatar with the mailbox link's dot at its foot, green
/// while linked, grey while not; no dot while the link is not known.
fn avatar_with_dot<'a>(
    a: &scene::Avatar,
    diameter: f32,
    link: Option<bool>,
    t: Colours,
) -> Element<'a, Message> {
    let Some(up) = link else {
        return avatar(a, diameter, t);
    };
    let ink = colour(match up {
        true => t.status_up,
        false => t.status_down,
    });
    face::with_dot(avatar(a, diameter, t), diameter, ink, colour(t.bar))
}

/// [`avatar`] for the window's layers: the profile's (1443).
pub(super) fn avatar_of<'a>(a: &scene::Avatar, diameter: f32, t: Colours) -> Element<'a, Message> {
    avatar(a, diameter, t)
}

/// A scene's avatar: a circle of `diameter` with its initials in its
/// colours, ringed by the liveness of the peer's position (1209).
fn avatar<'a>(a: &scene::Avatar, diameter: f32, t: Colours) -> Element<'a, Message> {
    let ring = a.ring.map(|ring| colour(ring.colour(&t)));
    face::avatar(a.initials.clone(), rgb(a.fg), rgb(a.bg), ring, diameter)
}

/// A bare button over `content`: the colours are the content's.
fn flat<'a>(content: impl Into<Element<'a, Message>>, on: Message) -> button::Button<'a, Message> {
    control::flat(content, on)
}

/// A word on the bars, as a link in the accent.
/// A filled button in the accent, ours' bubble ink on it (1307: Join,
/// Next, Create); without `on` it is dimmed and takes no press.
fn filled<'a>(words: &str, on: Option<Message>, t: Colours) -> Element<'a, Message> {
    let ground = colour(t.accent);
    let ink = colour(t.bubble_ours_text);
    let dim = on.is_none();
    button(text(words.to_string()).size(15).color(ink))
        .on_press_maybe(on)
        .padding([6, 18])
        .style(move |_: &Theme, _| button::Style {
            background: Some(Background::Color(match dim {
                true => ground.scale_alpha(0.4),
                false => ground,
            })),
            text_color: ink,
            border: border::rounded(size::COMPOSE_CORNER),
            ..button::Style::default()
        })
        .into()
}

fn link<'a>(words: &str, on: Message, t: Colours) -> Element<'a, Message> {
    control::link(words, on, colour(t.accent))
}

/// `levels` as a waveform `slots` bars wide, `width` by `height`, a
/// standing one folded by the recording's resampler.
fn wave<'a>(
    levels: &[u8],
    slots: usize,
    moving: bool,
    ink: Color,
    width: Length,
    height: f32,
) -> Element<'a, Message> {
    leviculum_ui_iced::wave::wave(
        levels,
        |levels, room| {
            loquax::ui::recording::LevelTrack::of(levels)
                .bars(room, loquax::ui::recording::Fold::Peak)
        },
        slots,
        moving,
        ink,
        width,
        height,
    )
}

impl App {
    /// The tokens of the theme the window is in.
    pub(super) fn tokens(&self) -> Colours {
        tokens(self.dark())
    }

    /// A catalogue word in the window's language.
    fn word(&self, id: StrId) -> &'static str {
        id.text(self.lang)
    }

    /// The scene's list as the window lists it: the rows its search finds,
    /// in the scene's order. What [`App::list_pane`] draws, and what the
    /// facade's rows are made of ([`scene::rows`]).
    pub(super) fn listed(&self) -> Vec<&Row> {
        self.seen
            .scene
            .list
            .as_ref()
            .map_or_else(Vec::new, |list| list.found(&self.search).collect())
    }

    /// The width the list takes: beside the chat a fixed one, alone the
    /// window's.
    fn list_width(&self) -> Length {
        match self.class() {
            LayoutClass::Wide => Length::Fixed(LIST_WIDTH),
            LayoutClass::Compact => Length::Fill,
        }
    }

    /// An action of a bar as a drawn icon with its label on hover; a
    /// disabled one is not drawn.
    fn icon_action<'a>(
        &self,
        icon: Icon,
        label: &str,
        on: Message,
        t: Colours,
    ) -> Element<'a, Message> {
        control::icon_action(icon, label, on, palette(t))
    }

    /// The scene's top bar over the list: the reader's avatar with the
    /// link's dot, the title, the bar's actions, and the gear. Light or
    /// dark is the settings' Appearance since 1450, no button of the bar.
    fn top_bar<'a>(&'a self, t: Colours) -> Element<'a, Message> {
        let top = &self.scene.top;
        let lead = match &top.lead {
            // A click on the reader's avatar raises what the scene says
            // it does: the profile (1443), as a tap does on the phone.
            Lead::Me { avatar, link, open } => {
                let face = avatar_with_dot(avatar, size::BAR_AVATAR, *link, t);
                Some(match open {
                    Some(open) => flat(face, Message::Intent(open.intent.clone())).into(),
                    None => face,
                })
            }
            Lead::Back { .. } => None,
        };
        let mut actions = Vec::new();
        for action in top.actions.iter().filter(|a| a.enabled) {
            let icon = match action.intent {
                Intent::Search => Icon::Search,
                Intent::NewConversation => Icon::Pencil,
                _ => continue,
            };
            actions.push(self.icon_action(
                icon,
                self.word(action.label_key),
                Message::Intent(action.intent.clone()),
                t,
            ));
        }
        // Fetch now (1457): the terminal's `fetch`, the phone's pull, for
        // a reader who will not wait for the next round.
        actions.push(self.icon_action(
            Icon::Fetch,
            self.word(Intent::Pull.label()),
            Message::Intent(Intent::Pull),
            t,
        ));
        actions.push(self.icon_action(
            Icon::Settings,
            self.word(StrId::Settings),
            Message::Settings,
            t,
        ));
        chrome::top_bar(
            lead,
            top.title.resolve(self.lang),
            actions,
            size::BAR,
            palette(t),
        )
    }

    /// One tab: its icon over its label, the unread count beside the
    /// icon, the active one in the accent on the selected row's ground.
    fn tab<'a>(&self, tab: &'a Tab, active: bool, t: Colours) -> Element<'a, Message> {
        let p = palette(t);
        let icon = match tab.id {
            TabId::Chats => Icon::Chats,
            TabId::Map => Icon::Map,
            // The Browse tab (1369): the shared icons have no globe yet,
            // and a page is what one looks for.
            TabId::Browse => Icon::Search,
            // The Rooms (1379): talk, as the chats, until a room icon is
            // drawn.
            TabId::Rooms => Icon::Chats,
            // Explore (1578): what one looks for, pages and rooms.
            TabId::Explore => Icon::Search,
        };
        let badge = tab.badge.map(|count| {
            chrome::badge(count, size::BADGE_TEXT - 2.0, size::BADGE_CORNER, p).padding([0, 5])
        });
        chrome::tab(
            icon,
            tab.label.resolve(self.lang),
            badge,
            active,
            Message::Intent(Intent::Activate(scene::NodeId::Tab(tab.id))),
            p,
        )
    }

    /// The tabs, the active one the window's screen: a rail at the left
    /// in the Wide class, a bar at the bottom in the Compact one; none
    /// when the scene has none (a stacked chat).
    pub(super) fn tabs(&self) -> Option<Element<'_, Message>> {
        let tabs = self.scene.tabs.as_ref()?;
        let t = self.tokens();
        let active = match self.screen {
            super::Screen::Chats | super::Screen::Settings => TabId::Chats,
            super::Screen::Map => TabId::Map,
            // The browser and a hub's rooms are opened from Explore.
            super::Screen::Browse | super::Screen::Rooms | super::Screen::Explore => TabId::Explore,
        };
        let items = tabs
            .items
            .iter()
            .map(|tab| self.tab(tab, tab.id == active, t));
        let rail = self.class() == LayoutClass::Wide;
        Some(chrome::tabs(items, rail, palette(t)))
    }

    /// The settings (1233, one structure since 1450, walk 1 finding 21):
    /// the nine sections of `loquax::ui::settings`, in the phone's order,
    /// each its title over what it holds; a press opens its page, Back
    /// goes to the list, then to the chats.
    pub(super) fn settings_pane(&self) -> Element<'_, Message> {
        use loquax::ui::settings::{self, Target};
        let t = self.tokens();
        let title = match self.settings_at {
            Some(section) => self.word(section.title()),
            None => self.word(StrId::Settings),
        };
        let bar = row![
            self.icon_action(
                Icon::Back,
                self.word(StrId::Back),
                Message::Intent(Intent::Back),
                t
            ),
            text(title)
                .size(22)
                .color(colour(t.text))
                .width(Length::Fill),
        ]
        .spacing(12)
        .align_y(Alignment::Center);
        let body: Element<'_, Message> = match self.settings_at {
            None => settings::entries(Target::Window)
                .into_iter()
                .fold(column![], |list, entry| {
                    let words = column![
                        text(entry.title.resolve(self.lang))
                            .size(16)
                            .color(colour(t.text)),
                        text(entry.summary.resolve(self.lang))
                            .size(14)
                            .color(colour(t.text_secondary)),
                    ]
                    .spacing(2);
                    list.push(chrome::list_item(
                        container(words).padding([10, 16]).width(Length::Fill),
                        false,
                        Message::Intent(entry.open.intent),
                        palette(t),
                    ))
                })
                .into(),
            Some(section) => self.settings_page(section, t),
        };
        container(
            column![
                container(bar)
                    .height(size::BAR)
                    .padding([0, 12])
                    .align_y(Alignment::Center),
                scrollable(body).height(Length::Fill),
            ]
            .width(Length::Fill),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_: &Theme| container::Style {
            background: Some(Background::Color(colour(t.background))),
            ..container::Style::default()
        })
        .into()
    }

    /// One section's page in the window: its switches as the window has
    /// them, the words the core says for the rest.
    fn settings_page(
        &self,
        section: loquax::ui::settings::Section,
        t: Colours,
    ) -> Element<'_, Message> {
        use loquax::ui::settings::Section;
        let say = |words: String| {
            container(text(words).size(15).color(colour(t.text)))
                .padding([12, 16])
                .width(Length::Fill)
        };
        let quiet = |words: String| {
            container(text(words).size(14).color(colour(t.text_secondary)))
                .padding([0, 16])
                .width(Length::Fill)
        };
        match section {
            Section::Profile => container(link(
                self.word(StrId::ProfileTitle),
                Message::Intent(Intent::Profile),
                t,
            ))
            .padding([12, 16])
            .into(),
            Section::Mailbox => self.mailbox_page(t),
            Section::Appearance => {
                let switch = self.switch_row(
                    self.word(StrId::SettingsDark),
                    self.word(StrId::Theme),
                    self.dark(),
                    |_| Message::Intent(Intent::Theme),
                    t,
                );
                column![switch].into()
            }
            Section::Chats => self.photo_rows(t),
            Section::Notifications => {
                say(self.word(StrId::SettingsNotificationsDesktop).to_string()).into()
            }
            Section::Privacy => self.switch_row(
                self.word(StrId::ReadReceipts),
                self.word(StrId::ReadReceiptsText),
                self.seen.receipts,
                |on| Message::Intent(Intent::ReadReceipts { on: Switch::of(on) }),
                t,
            ),
            Section::Maps => self.maps_rows(t),
            Section::Backup => say(self.word(StrId::SettingsBackupDesktop).to_string()).into(),
            Section::Advanced => column![
                say(self.word(StrId::PropagationNode).to_string()),
                quiet(self.word(StrId::PropagationNodeText).to_string()),
                self.hub_rows(t),
                container(link(
                    self.word(StrId::Help),
                    Message::Intent(Intent::Help),
                    t
                ))
                .padding([12, 16]),
            ]
            .into(),
        }
    }

    /// The public rooms' hub (1577, 1599), the core's lines: which hub,
    /// the hubs heard and shipped with how far each is and a mark on the
    /// one in force, a press choosing it, "More…" for every hub in the
    /// page's scroll, none, the field for another's address, and what a
    /// hub learns.
    /// The mailbox page (1642), the core's scene: a header per group in
    /// the accent, a line per line with its value under it, a note
    /// smaller, the address selectable and with "Copy", "Show my code" a
    /// link to the profile.
    fn mailbox_page(&self, t: Colours) -> Element<'_, Message> {
        let lang = self.lang;
        let Some(page) = &self.seen.mailbox else {
            return column![].into();
        };
        let mut rows = column![].spacing(2).padding([4, 16]);
        for group in &page.groups {
            rows = rows.push(
                container(
                    text(group.title.resolve(lang))
                        .size(14)
                        .color(colour(t.accent)),
                )
                .padding(iced::Padding {
                    top: 12.0,
                    bottom: 4.0,
                    ..iced::Padding::ZERO
                }),
            );
            for line in &group.lines {
                let label = line.label.resolve(lang);
                if line.note {
                    rows = rows.push(text(label).size(13).color(colour(t.text_secondary)));
                    continue;
                }
                if let Some(action) = &line.action {
                    rows = rows.push(link(&label, Message::Intent(action.intent.clone()), t));
                    continue;
                }
                let mut words = column![text(label).size(15).color(colour(t.text))];
                if let Some(value) = &line.value {
                    words = words.push(
                        text(value.resolve(lang))
                            .size(14)
                            .color(colour(t.text_secondary)),
                    );
                }
                let mut shown = row![container(words).width(Length::Fill)]
                    .spacing(8)
                    .align_y(Alignment::Center);
                if let Some(copy) = &line.copy {
                    shown = shown.push(filled(
                        &Text::key(StrId::MailboxCopy).resolve(lang),
                        Some(Message::CopyText(copy.clone())),
                        t,
                    ));
                }
                rows = rows.push(container(shown).padding([4, 0]));
            }
        }
        rows.into()
    }

    fn hub_rows(&self, t: Colours) -> Element<'_, Message> {
        use loquax::ui::intent::Intent;
        use loquax::ui::rooms::HubPick;
        let lang = self.lang;
        let mut rows = column![].spacing(4).padding([12, 16]);
        for row in &self.seen.hubs {
            let label = row.label.resolve(lang);
            let value = row.value.as_ref().map(|v| v.resolve(lang));
            match row.action.as_ref().map(|a| &a.intent) {
                Some(Intent::RoomsHub(HubPick::Paste)) => {
                    let words = match value {
                        Some(own) => format!("● {label}: {own}"),
                        None => format!("○ {label}"),
                    };
                    rows = rows.push(text(words).size(15).color(colour(t.text)));
                    let field = text_input(
                        &Text::key(StrId::RoomsHubPasteHint).resolve(lang),
                        &self.hub_field,
                    )
                    .id(HUB_FIELD)
                    .on_input(Message::HubTyped)
                    .on_submit(Message::HubTake)
                    .size(15)
                    .padding([6, 10]);
                    rows = rows.push(
                        row![
                            field,
                            button(text("OK").size(15)).on_press(Message::HubTake)
                        ]
                        .spacing(8)
                        .align_y(Alignment::Center),
                    );
                }
                Some(Intent::RoomsHub(HubPick::More)) => {
                    rows = rows.push(link(
                        &label,
                        Message::Intent(Intent::RoomsHub(HubPick::More)),
                        t,
                    ));
                }
                Some(intent) => {
                    // The core marks the hub in force with a check at the
                    // value's end; the window says it with a filled dot.
                    let value = value.unwrap_or_default();
                    let said = value
                        .trim_end_matches('\u{2713}')
                        .trim_end_matches(loquax::ui::strings::JOIN)
                        .trim();
                    let mark = if value.ends_with('\u{2713}') {
                        "●"
                    } else {
                        "○"
                    };
                    let words = match said {
                        "" => format!("{mark} {label}"),
                        said => format!("{mark} {label} · {said}"),
                    };
                    rows = rows.push(link(&words, Message::Intent(intent.clone()), t));
                }
                None if value.is_some() => {
                    let words = format!("{label}: {}", value.unwrap_or_default());
                    rows = rows.push(text(words).size(16).color(colour(t.text)));
                }
                None => {
                    rows = rows.push(text(label).size(14).color(colour(t.text_secondary)));
                }
            }
        }
        rows.into()
    }

    /// A row of words and a switch, on in the accent as the phone's.
    fn switch_row(
        &self,
        title: &str,
        line: &str,
        on: bool,
        toggled: fn(bool) -> Message,
        t: Colours,
    ) -> Element<'_, Message> {
        let words = column![
            text(title.to_string()).size(16).color(colour(t.text)),
            text(line.to_string())
                .size(14)
                .color(colour(t.text_secondary)),
        ]
        .spacing(2)
        .width(Length::Fill);
        let switch = toggler(on)
            .on_toggle(toggled)
            .style(move |theme: &Theme, status| {
                let mut style = toggler::default(theme, status);
                // Every state toggled: before its first event iced calls
                // it disabled, which a first frame and the golden draw.
                if let toggler::Status::Active { is_toggled: true }
                | toggler::Status::Hovered { is_toggled: true }
                | toggler::Status::Disabled { is_toggled: true } = status
                {
                    style.background = Background::Color(colour(t.accent));
                }
                style
            });
        row![words, switch]
            .spacing(12)
            .align_y(Alignment::Center)
            .padding([12, 16])
            .into()
    }

    /// The Rooms tab (1379), a room as a chat (1450, walk 1 finding 20):
    /// the rooms as list rows with their avatars beside the open room,
    /// whose bar is a chat's (avatar, name, the people seen under it, a
    /// press on it shows them, the menu with "Leave room"), its lines as
    /// bubbles (another's run named once with the writer's avatar, ours
    /// on the right in the accent) and the comings and goings as quiet
    /// lines in the middle; the compose field with its hint once, and a
    /// line under it only for a toast.
    pub(super) fn rooms_pane(&self) -> Element<'_, Message> {
        use loquax::ui::rooms::RoomsEvent;
        let Some(node) = &self.seen.scene.rooms else {
            return Space::new().into();
        };
        let t = self.tokens();
        let lang = self.lang;
        let ink = colour(t.text);
        let quiet = colour(t.text_secondary);
        let rows = node
            .rows
            .iter()
            .enumerate()
            .fold(column![].spacing(2), |list, (i, room)| {
                // A room is public (1577): the globe and the word beside
                // its name.
                let mut line = row![
                    avatar(&room.avatar, size::BAR_AVATAR, t),
                    text(room.name.clone()).size(size::ROW_NAME).color(ink),
                    text(format!(
                        "{} {}",
                        loquax::ui::rooms::PUBLIC_MARK,
                        room.public.resolve(lang)
                    ))
                    .size(13)
                    .color(quiet)
                    .width(Length::Fill),
                ]
                .spacing(12)
                .align_y(Alignment::Center);
                if room.unread > 0 {
                    line = line.push(chrome::badge(
                        room.unread,
                        size::BADGE_TEXT,
                        size::BADGE_CORNER,
                        palette(t),
                    ));
                }
                list.push(chrome::list_item(
                    container(line).padding([8, 12]),
                    room.selected,
                    Message::Rooms(RoomsEvent::Show(Some(i))),
                    palette(t),
                ))
            });
        let mut left = column![].spacing(6);
        if let Some(status) = &node.status {
            left = left
                .push(container(text(status.resolve(lang)).size(14).color(quiet)).padding([8, 12]));
        }
        // "Connect" or "Try again" under the state (1652).
        if let Some(retry) = &node.retry {
            left = left.push(
                container(
                    button(text(retry.resolve(lang)).size(14))
                        .on_press(Message::Rooms(RoomsEvent::Retry))
                        .style(button::text),
                )
                .padding([0, 12]),
            );
        }
        let left = container(left.push(scrollable(rows).height(Length::Fill)))
            .width(Length::Fixed(260.0))
            .height(Length::Fill)
            .padding([8, 0]);
        let room: Element<'_, Message> = match &node.open {
            Some(open) => self.room_chat(open, t),
            // The rooms the hub names (1652): a press joins one and opens
            // it; the field below is the way to another.
            None if node.listed.is_some() => {
                let mut list = column![].spacing(2);
                if let Some(listed) = &node.listed {
                    list = list.push(
                        container(
                            text(listed.title.resolve(lang))
                                .size(size::ROW_NAME)
                                .color(ink),
                        )
                        .padding([12, 16]),
                    );
                    for name in &listed.rooms {
                        let line = row![
                            avatar(&loquax::ui::rooms::avatar(name), size::BAR_AVATAR, t),
                            text(name.clone()).size(size::ROW_NAME).color(ink),
                        ]
                        .spacing(12)
                        .align_y(Alignment::Center);
                        list = list.push(chrome::list_item(
                            container(line).padding([8, 12]),
                            false,
                            Message::Rooms(RoomsEvent::Join(name.clone())),
                            palette(t),
                        ));
                    }
                    if let Some(note) = &listed.note {
                        list = list.push(
                            container(text(note.resolve(lang)).size(14).color(quiet))
                                .padding([8, 16]),
                        );
                    }
                    if let Some(other) = &listed.other {
                        list = list.push(
                            container(text(other.resolve(lang)).size(13).color(quiet))
                                .padding([12, 16]),
                        );
                    }
                }
                if let Some(empty) = &node.empty {
                    list = list.push(
                        container(text(empty.resolve(lang)).size(15).color(quiet)).padding([8, 16]),
                    );
                }
                scrollable(list).height(Length::Fill).into()
            }
            None => container(
                text(
                    node.empty
                        .as_ref()
                        .map(|e| e.resolve(lang))
                        .unwrap_or_default(),
                )
                .size(17)
                .color(quiet),
            )
            .center(Length::Fill)
            .into(),
        };
        let field = text_input(&node.placeholder.resolve(lang), &node.input)
            .on_input(|typed| Message::Rooms(RoomsEvent::Typed(typed)))
            .on_submit(Message::Rooms(RoomsEvent::Submit))
            .size(size::BODY)
            .padding([10, 16])
            .style(move |theme: &Theme, status| {
                let mut style = text_input::default(theme, status);
                style.background = Background::Color(colour(t.bar));
                style.border = border::rounded(size::COMPOSE_CORNER);
                style
            });
        let mut right = column![room, container(field).padding([8, 12])];
        // A toast in the window's notice; nothing else under the field:
        // its hint is said in it, once.
        if let Some(notice) = &self.seen.notice {
            right =
                right.push(container(text(notice.clone()).size(13).color(quiet)).padding([0, 16]));
        }
        container(row![left, right.width(Length::Fill)].height(Length::Fill))
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_: &Theme| container::Style {
                background: Some(Background::Color(colour(t.background))),
                ..container::Style::default()
            })
            .into()
    }

    /// The Explore tab (1578): the pages and the public rooms under one
    /// button, one scroll. On top the search field, which also takes a
    /// pasted address, and the order's toggle; the pages under their
    /// headings, a press opens one, the star bookmarks it; then the rooms
    /// the reader is in, the hubs Loquax ships and the reader's own, and
    /// the hubs heard on the mesh. Each section says in one sentence when
    /// it has nothing.
    pub(super) fn explore_pane(&self) -> Element<'_, Message> {
        use loquax::ui::explore::ExploreEvent;
        let Some(node) = &self.seen.scene.explore else {
            return Space::new().into();
        };
        let t = self.tokens();
        let p = palette(t);
        let lang = self.lang;
        let ink = colour(t.text);
        let quiet = colour(t.text_secondary);
        let heading =
            |words: String| container(text(words).size(13).color(quiet)).padding([10, 16]);
        if let Some(sheet) = &node.sheet {
            return self.server_sheet(sheet);
        }
        let pages = &node.pages;
        let field = text_input(&pages.placeholder.resolve(lang), &pages.query)
            .on_input(|typed| Message::Explore(ExploreEvent::Typed(typed)))
            .on_submit(Message::Explore(ExploreEvent::OpenAddress))
            .size(size::BODY)
            .padding([10, 16])
            .style(move |theme: &Theme, status| {
                let mut style = text_input::default(theme, status);
                style.background = Background::Color(colour(t.bar));
                style.border = border::rounded(size::COMPOSE_CORNER);
                style
            });
        let sort = button(text(pages.sort.resolve(lang)).size(13))
            .on_press(Message::Explore(ExploreEvent::Az(!pages.az)))
            .style(button::text);
        let mut list = column![].spacing(2);
        // How old the mailbox's list is, on top (1644).
        if let Some(as_of) = &pages.as_of {
            list = list.push(heading(as_of.resolve(lang)));
        }
        list = list
            .push(
                container(
                    text(pages.title.resolve(lang))
                        .size(size::ROW_NAME)
                        .color(ink),
                )
                .padding([12, 16]),
            )
            .push(
                container(row![field, sort].spacing(8).align_y(Alignment::Center)).padding([0, 12]),
            );
        if let Some(off) = &pages.off {
            list = list.push(heading(off.resolve(lang)));
        }
        if let Some((url, _)) = &pages.address {
            let words = Text::Key(
                StrId::ExploreOpenAddress,
                Vec::from([Arg::Str(url.clone())]),
            );
            list = list.push(chrome::list_item(
                container(text(words.resolve(lang)).size(size::ROW_NAME).color(ink))
                    .padding([8, 16]),
                false,
                Message::Explore(ExploreEvent::OpenAddress),
                p,
            ));
        }
        for group in &pages.groups {
            list = list.push(heading(group.heading.resolve(lang)));
            for page in &group.rows {
                let mut said = Vec::new();
                if let Some(seen) = &page.seen {
                    said.push(seen.resolve(lang));
                }
                if let Some(hops) = &page.hops {
                    said.push(hops.resolve(lang));
                }
                let star = button(
                    text(if page.bookmarked {
                        "\u{2605}"
                    } else {
                        "\u{2606}"
                    })
                    .size(18),
                )
                .on_press(Message::Explore(ExploreEvent::Bookmark(page.dest)))
                .style(button::text);
                let line = row![
                    column![
                        text(page.label.resolve(lang))
                            .size(size::ROW_NAME)
                            .color(ink),
                        text(said.join(" · ")).size(13).color(quiet),
                    ]
                    .width(Length::Fill),
                    star,
                ]
                .spacing(12)
                .align_y(Alignment::Center);
                list = list.push(chrome::list_item(
                    container(line).padding([6, 16]),
                    // The terminal's cursor is the terminal's: the pointer
                    // marks nothing here.
                    false,
                    Message::Explore(ExploreEvent::OpenPage(page.dest)),
                    p,
                ));
            }
        }
        if let Some(empty) = &pages.empty {
            list = list.push(heading(empty.resolve(lang)));
        }
        let rooms = &node.rooms;
        list = list.push(
            container(
                text(rooms.title.resolve(lang))
                    .size(size::ROW_NAME)
                    .color(ink),
            )
            .padding([16, 16]),
        );
        for (i, room) in rooms.joined.iter().enumerate() {
            let mut line = row![
                avatar(&room.avatar, size::BAR_AVATAR, t),
                text(room.name.clone()).size(size::ROW_NAME).color(ink),
                text(format!(
                    "{} {}",
                    loquax::ui::rooms::PUBLIC_MARK,
                    room.public.resolve(lang)
                ))
                .size(13)
                .color(quiet)
                .width(Length::Fill),
            ]
            .spacing(12)
            .align_y(Alignment::Center);
            if room.unread > 0 {
                line = line.push(chrome::badge(
                    room.unread,
                    size::BADGE_TEXT,
                    size::BADGE_CORNER,
                    p,
                ));
            }
            list = list.push(chrome::list_item(
                container(line).padding([8, 16]),
                false,
                Message::Explore(ExploreEvent::OpenRoom(i)),
                p,
            ));
        }
        let hub = |hub: &loquax::ui::explore::HubRow| {
            let mut words = hub.name.resolve(lang);
            if hub.chosen {
                words.push_str(" \u{2713}");
            }
            // A server's row as a page's (1618): the name over when it was
            // heard and how far.
            let said = hub
                .seen
                .iter()
                .chain(hub.value.iter())
                .map(|v| v.resolve(lang))
                .collect::<Vec<_>>()
                .join(" · ");
            chrome::list_item(
                container(
                    column![
                        text(words).size(size::ROW_NAME).color(ink),
                        text(said).size(13).color(quiet),
                    ]
                    .width(Length::Fill),
                )
                .padding([6, 16]),
                false,
                Message::Explore(ExploreEvent::Hub(hub.hash)),
                p,
            )
        };
        if let Some(empty) = &rooms.joined_empty {
            list = list.push(heading(empty.resolve(lang)));
        }
        // The rooms screen of the server in use (1655).
        if let Some(open) = &rooms.open_rooms {
            list = list.push(
                container(
                    button(text(open.resolve(lang)).size(size::BODY))
                        .on_press(Message::Explore(ExploreEvent::OpenRooms)),
                )
                .padding([6, 16]),
            );
        }
        // The servers under their own heading (1621).
        list = list.push(heading(rooms.servers_title.resolve(lang)));
        for row in &rooms.hubs {
            list = list.push(hub(row));
        }
        if let Some(empty) = &rooms.servers_empty {
            list = list.push(heading(empty.resolve(lang)));
        }
        if let Some(notice) = &self.seen.notice {
            list = list.push(heading(notice.clone()));
        }
        container(scrollable(list.padding([0, 0])).height(Length::Fill))
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_: &Theme| container::Style {
                background: Some(Background::Color(colour(t.background))),
                ..container::Style::default()
            })
            .into()
    }

    /// A server's sheet over Explore (1618): its name, when it was heard
    /// and how far, the one button that makes it the public rooms' server
    /// with what that tells it, and the rooms it named, a press joining
    /// one. Nothing is chosen by opening it.
    fn server_sheet<'a>(
        &'a self,
        sheet: &'a loquax::ui::explore::HubSheet,
    ) -> Element<'a, Message> {
        use loquax::ui::explore::ExploreEvent;
        let t = self.tokens();
        let p = palette(t);
        let lang = self.lang;
        let ink = colour(t.text);
        let quiet = colour(t.text_secondary);
        let said = sheet
            .seen
            .iter()
            .chain(sheet.far.iter())
            .map(|v| v.resolve(lang))
            .collect::<Vec<_>>()
            .join(" · ");
        let mut list = column![
            row![
                text(sheet.name.resolve(lang))
                    .size(size::BAR_NAME)
                    .color(ink)
                    .width(Length::Fill),
                button(text(sheet.close.resolve(lang)).size(13))
                    .on_press(Message::Explore(ExploreEvent::CloseSheet))
                    .style(button::text),
            ]
            .align_y(Alignment::Center)
            .padding([12, 16]),
            container(text(said).size(13).color(quiet)).padding([0, 16]),
        ]
        .spacing(6);
        if sheet.chosen {
            list = list.push(
                container(text(sheet.button.resolve(lang)).size(size::BODY).color(ink))
                    .padding([10, 16]),
            );
            // Its rooms screen (1655).
            if let Some(open) = &sheet.open_rooms {
                list = list.push(
                    container(
                        button(text(open.resolve(lang)).size(size::BODY))
                            .on_press(Message::Explore(ExploreEvent::OpenRooms)),
                    )
                    .padding([0, 16]),
                );
            }
        } else {
            list = list.push(
                container(
                    button(text(sheet.button.resolve(lang)).size(size::BODY))
                        .on_press(Message::Explore(ExploreEvent::UseHub(sheet.hash))),
                )
                .padding([10, 16]),
            );
            list = list.push(
                container(text(sheet.notice.resolve(lang)).size(13).color(quiet)).padding([0, 16]),
            );
        }
        list = list.push(
            container(
                text(sheet.rooms_title.resolve(lang))
                    .size(size::ROW_NAME)
                    .color(ink),
            )
            .padding([12, 16]),
        );
        for room in &sheet.rooms {
            list = list.push(chrome::list_item(
                container(text(room.clone()).size(size::ROW_NAME).color(ink)).padding([8, 16]),
                false,
                Message::Explore(ExploreEvent::JoinRoom(sheet.hash, room.clone())),
                p,
            ));
        }
        if let Some(none) = &sheet.rooms_note {
            list = list
                .push(container(text(none.resolve(lang)).size(13).color(quiet)).padding([4, 16]));
        }
        container(scrollable(list).height(Length::Fill))
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_: &Theme| container::Style {
                background: Some(Background::Color(colour(t.background))),
                ..container::Style::default()
            })
            .into()
    }

    /// The open room as a chat: its bar, its people or its lines.
    fn room_chat<'a>(
        &'a self,
        open: &'a loquax::ui::rooms::RoomView,
        t: Colours,
    ) -> Element<'a, Message> {
        use loquax::ui::rooms::{RoomLineKind, RoomsEvent};
        let lang = self.lang;
        let ink = colour(t.text);
        let quiet = colour(t.text_secondary);
        let people_shown = open.people.is_some();
        // The bar: a press on the avatar and the names shows the people,
        // as a group's header does; the menu's verbs beside.
        let head = flat(
            row![
                avatar(&open.avatar, size::BAR_AVATAR, t),
                column![
                    text(open.name.clone()).size(size::BAR_NAME).color(ink),
                    text(open.subtitle.resolve(lang)).size(13).color(quiet),
                ]
                .spacing(2),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
            Message::Rooms(RoomsEvent::People(!people_shown)),
        )
        .width(Length::Fill);
        let mut bar = row![head].spacing(12).align_y(Alignment::Center);
        for verb in &open.menu {
            bar = bar.push(link(
                &verb.label.resolve(lang),
                Message::Rooms(verb.event.clone()),
                t,
            ));
        }
        let bar = container(bar)
            .height(size::BAR)
            .padding([0, 12])
            .align_y(Alignment::Center)
            .width(Length::Fill)
            .style(move |_: &Theme| container::Style {
                background: Some(Background::Color(colour(t.bar))),
                ..container::Style::default()
            });
        if let Some(people) = &open.people {
            let list = people.iter().fold(
                column![text(open.people_note.resolve(lang)).size(14).color(quiet)].spacing(10),
                |list, who| {
                    let name = who.resolve(lang);
                    list.push(
                        row![
                            avatar(&loquax::ui::rooms::avatar(&name), size::BAR_AVATAR, t),
                            text(name).size(size::ROW_NAME).color(ink),
                        ]
                        .spacing(12)
                        .align_y(Alignment::Center),
                    )
                },
            );
            return column![
                bar,
                scrollable(container(list).padding([16, 16])).height(Length::Fill)
            ]
            .into();
        }
        let width = self.chat_width() - 260.0;
        let note = |words: String| {
            container(text(words).size(size::DAY).color(quiet))
                .center_x(Length::Fill)
                .padding(iced::Padding::ZERO.top(8).bottom(4))
        };
        let mut list = column![note(open.live.resolve(lang))].padding([0, 12]);
        for line in &open.lines {
            if !line.bubble {
                let said = line.text.resolve(lang);
                list = list.push(note(match line.kind {
                    RoomLineKind::Error => format!("! {said}"),
                    _ => said,
                }));
                continue;
            }
            let ours = line.ours;
            let (ground, ink, meta) = match ours {
                true => (t.bubble_ours, t.bubble_ours_text, t.bubble_ours_meta),
                false => (t.bubble_theirs, t.bubble_theirs_text, t.bubble_theirs_meta),
            };
            let (ground, ink, meta) = (colour(ground), colour(ink), colour(meta));
            list = list.push(Space::new().height(match line.named || ours {
                true => size::GAP,
                false => size::GAP_GROUPED,
            }));
            if line.named {
                list = list.push(
                    container(
                        text(line.who.resolve(lang))
                            .size(13)
                            .color(colour(t.accent)),
                    )
                    .padding(iced::Padding::ZERO.left(ROOM_FACE + 8.0)),
                );
            }
            let action = matches!(
                line.kind,
                RoomLineKind::Said(leviculum_rrc::LineKind::Action)
            );
            let said = match action {
                true => format!("{} {}", line.who.resolve(lang), line.text.resolve(lang)),
                false => line.text.resolve(lang),
            };
            let mut words = text(said).size(size::BODY).color(ink);
            if action {
                words = words.font(iced::Font {
                    style: iced::font::Style::Italic,
                    ..iced::Font::DEFAULT
                });
            }
            let mut inside = column![words].spacing(4);
            if let Some(time) = &line.time {
                inside = inside.push(
                    container(text(time.clone()).size(size::META).color(meta))
                        .align_right(Length::Shrink),
                );
            }
            let shape = container(inside)
                .padding([8, 12])
                .max_width(width * size::BUBBLE_PART)
                .style(move |_: &Theme| container::Style {
                    background: Some(Background::Color(ground)),
                    border: border::rounded(size::CORNER),
                    ..container::Style::default()
                });
            // Another's bubble stands beside its writer's avatar, the
            // first of a run; the rest of the run keeps its place.
            let face: Element<'_, Message> = match (&line.avatar, ours) {
                (_, true) => Space::new().into(),
                (Some(a), false) => avatar(a, ROOM_FACE, t),
                (None, false) => Space::new().width(ROOM_FACE).into(),
            };
            let side: Element<'_, Message> = match ours {
                true => container(shape).align_right(Length::Fill).into(),
                false => row![face, shape]
                    .spacing(8)
                    .align_y(Alignment::Start)
                    .into(),
            };
            list = list.push(side);
        }
        list = list.push(Space::new().height(size::GAP));
        column![
            bar,
            soft(
                scrollable(list)
                    .anchor_bottom()
                    .height(Length::Fill)
                    .width(Length::Fill)
            ),
        ]
        .into()
    }

    /// The phone's photo size in the window (1371): its title and the
    /// three tiers, the one in use marked; a press sets another.
    fn photo_rows(&self, t: Colours) -> Element<'_, Message> {
        use loquax::ui::photos::PhotoSize;
        let mut rows = column![text(self.word(StrId::PhotoSize))
            .size(16)
            .color(colour(t.text))]
        .spacing(4)
        .padding([12, 16]);
        for size in PhotoSize::ALL {
            let words = match size {
                PhotoSize::Small => StrId::PhotoSmall,
                PhotoSize::Phone => StrId::PhotoPhone,
                PhotoSize::Original => StrId::PhotoOriginal,
            };
            let mark = match self.seen.photo_size == size {
                true => "●",
                false => "○",
            };
            rows = rows.push(link(
                &format!("{mark} {}", self.word(words)),
                Message::Intent(Intent::PhotoSize(size)),
                t,
            ));
        }
        rows.into()
    }

    /// The phone's Maps page in the window (1290): its title, the storage
    /// the regions take, and "Delete all downloaded maps (1.2 GB)", off
    /// without maps; a press asks once, in the words that say whether the
    /// autoload brings them back, with "Delete all" and "Cancel".
    fn maps_rows(&self, t: Colours) -> Element<'_, Message> {
        let (count, bytes) = self.maps_usage;
        let size = Arg::Str(loquax::ui::photos::size_words(bytes));
        let say = |id: StrId, arg: Arg| Text::Key(id, vec![arg]).resolve(self.lang);
        let title = text(self.word(StrId::SettingsMaps))
            .size(16)
            .color(colour(t.text));
        let used = text(say(StrId::MapsUsed, size.clone()))
            .size(14)
            .color(colour(t.text_secondary));
        let ink = match count {
            0 => t.text_secondary,
            _ => t.failed_mark_on_panel,
        };
        let all = button(
            text(say(StrId::DeleteAllRegions, size))
                .size(15)
                .color(colour(ink)),
        )
        .on_press_maybe(
            (count > 0).then_some(Message::Intent(Intent::DeleteAllRegions {
                confirmed: false,
            })),
        )
        .padding([4, 0])
        .style(|_: &Theme, _| button::Style::default());
        let mut rows = column![title, used, all].spacing(4).padding([12, 16]);
        if self.maps_asked {
            let ask = match self.seen.autoload {
                true => StrId::DeleteAllRegionsAsk,
                false => StrId::DeleteAllRegionsAskManual,
            };
            let count = i64::try_from(count).unwrap_or(i64::MAX);
            let yes = flat(
                text(self.word(StrId::DeleteAllRegionsYes))
                    .size(15)
                    .color(colour(t.failed_mark_on_panel)),
                Message::Intent(Intent::DeleteAllRegions { confirmed: true }),
            )
            .padding([4, 8]);
            rows = rows.push(
                text(say(ask, Arg::Int(count)))
                    .size(14)
                    .color(colour(t.text)),
            );
            rows = rows.push(row![
                yes,
                link(self.word(StrId::Cancel), Message::Intent(Intent::Back), t)
            ]);
        }
        rows.into()
    }

    /// The list pane: the scene's top bar, the search, the new
    /// conversation's field when asked for, and the rows.
    pub(super) fn list_pane(&self) -> Element<'_, Message> {
        let t = self.tokens();
        let bar = self.top_bar(t);
        let field = |value: &str, hint: &str| {
            chrome::search_field(hint, value, size::COMPOSE_CORNER, palette(t))
        };
        let search = field(&self.search, self.word(StrId::Search))
            .id(SEARCH_FIELD)
            .on_input(Message::Search);
        let mut pane = column![bar, container(search).padding([0, 12])].spacing(8);
        if let Some(address) = &self.new_address {
            let new = field(address, "Address, 32 hex digits")
                .on_input(Message::NewAddress)
                .on_submit(Message::NewGo);
            pane = pane.push(container(new).padding([0, 12]));
            // A contact code as a picture, the desktop's camera (1341).
            pane = pane.push(
                container(link(super::code_title(self.lang), Message::PickCode, t))
                    .padding([0, 12]),
            );
            // "New group" at the top of the new chat (1307, study 4.1).
            pane = pane.push(
                container(link(
                    self.word(StrId::NewGroup),
                    Message::Intent(Intent::Group(GroupVerb::New)),
                    t,
                ))
                .padding([0, 12]),
            );
        }
        let empty = (self.scene.list.as_ref()).is_none_or(|list| list.rows.is_empty());
        let picked = self.picked_ranges();
        // Only the rows on screen are built and laid out (1487): a row
        // and, under the one whose menu is open, its menu.
        let listed = self.listed();
        let lang = stamp_of(std::mem::discriminant(&self.lang));
        let mut lines: Vec<(&Row, bool)> = Vec::with_capacity(listed.len());
        let mut items = Vec::with_capacity(listed.len());
        for r in listed {
            lines.push((r, false));
            items.push(Item {
                key: stamp_of((r.id, false)),
                stamp: row_stamp(r) ^ lang,
            });
            if self.menu == Some(r.id) {
                lines.push((r, true));
                items.push(Item {
                    key: stamp_of((r.id, true)),
                    stamp: lang,
                });
            }
        }
        let rows = rows(items, move |i| match lines[i] {
            (r, true) => self.row_actions(r, t),
            // A right click opens the row's menu (1371), as a long press
            // does on the phone.
            (r, false) => mouse_area(list_row(
                r,
                self.seen.now,
                self.lang,
                t,
                &picked,
                self.picking,
            ))
            .on_right_press(Message::Intent(Intent::Menu(Some(r.id))))
            .into(),
        });
        let words = (self.scene.list.as_ref()).map(|list| list.empty.resolve(self.lang));
        let body: Element<'_, Message> = match empty {
            true => container(
                text(words.unwrap_or_default())
                    .size(15)
                    .color(colour(t.text_secondary)),
            )
            .padding(16)
            .into(),
            false => soft_picking(scrollable(rows).height(Length::Fill), self.picking),
        };
        container(pane.push(body))
            .width(self.list_width())
            .height(Length::Fill)
            .style(move |_: &Theme| container::Style {
                background: Some(Background::Color(colour(t.background))),
                ..container::Style::default()
            })
            .into()
    }

    /// The chat pane: the open conversation, or a word when none is.
    pub(super) fn chat_pane(&self) -> Element<'_, Message> {
        let t = self.tokens();
        let ground = move |_: &Theme| container::Style {
            background: Some(Background::Color(colour(t.background))),
            ..container::Style::default()
        };
        let Some(open) = &self.scene.chat else {
            return container(
                text("Pick a conversation")
                    .size(17)
                    .color(colour(t.text_secondary)),
            )
            .center(Length::Fill)
            .style(ground)
            .into();
        };
        let bar = self.chat_bar(open, t);
        let messages = self.groups(open, t);
        let mut pane = column![bar];
        // Our position share running here (1457): the phone's chip, its
        // time left and its Stop.
        if let Some(chip) = &open.share {
            pane = pane.push(self.share_chip(chip, t));
        }
        let mut pane = pane.push(messages);
        // A group the reader is only invited to: its card above the
        // compose bar (1307).
        if let Some(invite) = &open.invite {
            pane = pane.push(container(self.invite_card(invite, false, t)).padding([8, 12]));
        }
        // A group its admin deleted (1307 pass four): its line where the
        // compose bar was; nothing can be written to it.
        let bottom: Element<'_, Message> = match &open.ended {
            Some(ended) => container(
                text(ended.resolve(self.lang))
                    .size(15)
                    .color(colour(t.text_secondary)),
            )
            .center_x(Length::Fill)
            .padding([14, 12])
            .into(),
            None => self.compose_bar(open, t),
        };
        pane.push(bottom)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    /// The chip under the chat's bar while our position share runs there
    /// (1457): "Sharing your position, 42 min left" and Stop, one click.
    fn share_chip<'a>(&self, chip: &'a scene::ShareChip, t: Colours) -> Element<'a, Message> {
        let line = row![
            text(chip.words.resolve(self.lang))
                .size(14)
                .color(colour(t.text))
                .width(Length::Fill),
            link(
                self.word(chip.stop.label_key),
                Message::Intent(chip.stop.intent.clone()),
                t,
            ),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        container(
            container(line)
                .padding([4, 12])
                .style(move |_: &Theme| container::Style {
                    background: Some(Background::Color(colour(t.panel))),
                    border: Border {
                        radius: border::radius(16),
                        width: 1.0,
                        color: colour(t.accent),
                    },
                    ..container::Style::default()
                }),
        )
        .padding([6, 12])
        .width(Length::Fill)
        .into()
    }

    /// Whether a group's screen is open (1307): "New group" or a member
    /// sheet.
    pub(super) fn group_screen(&self) -> bool {
        self.scene.new_group.is_some() || self.scene.members.is_some()
    }

    /// "New group" or the member sheet, as the scene has them.
    pub(super) fn group_pane(&self) -> Element<'_, Message> {
        let t = self.tokens();
        let ink = colour(t.text);
        let quiet = colour(t.text_secondary);
        let mut pane = column![].spacing(10).padding([16, 16]);
        if let Some(new) = &self.scene.new_group {
            let head = row![
                text(new.title.resolve(self.lang))
                    .size(size::BAR_NAME)
                    .color(ink)
                    .width(Length::Fill),
                link(
                    self.word(new.close.label_key),
                    Message::Intent(new.close.intent.clone()),
                    t
                ),
            ]
            .align_y(Alignment::Center);
            pane = pane.push(head);
            if new.step == GroupStep::Name {
                let field = chrome::search_field(
                    &new.hint.resolve(self.lang),
                    &new.name,
                    size::COMPOSE_CORNER,
                    palette(t),
                )
                .on_input(Message::GroupName)
                .on_submit(Message::Intent(new.create.intent.clone()));
                pane = pane.push(field);
                pane = pane.push(text(new.trust.resolve(self.lang)).size(14).color(quiet));
            } else {
                pane = pane.push(text(new.picked.resolve(self.lang)).size(14).color(quiet));
            }
            let mut rows = column![].spacing(2);
            for r in &new.rows {
                let tick = if r.picked { "☑" } else { "☐" };
                let line = row![
                    text(tick).size(18).color(ink),
                    avatar(&r.avatar, size::ROW_AVATAR, t),
                    text(r.name.clone()).size(size::ROW_NAME).color(ink),
                ]
                .spacing(size::ROW_GAP)
                .align_y(Alignment::Center);
                rows = rows.push(match new.step {
                    GroupStep::Pick => chrome::list_item(
                        line,
                        r.picked,
                        Message::Intent(r.toggle.intent.clone()),
                        palette(t),
                    ),
                    GroupStep::Name => line.into(),
                });
            }
            pane = pane.push(soft(scrollable(rows).height(Length::Fill)));
            let go = match new.step {
                GroupStep::Pick => &new.next,
                GroupStep::Name => &new.create,
            };
            pane = pane.push(filled(
                self.word(go.label_key),
                go.enabled.then(|| Message::Intent(go.intent.clone())),
                t,
            ));
        } else if let Some(sheet) = &self.scene.members {
            let head = row![
                column![
                    text(sheet.title.resolve(self.lang))
                        .size(size::BAR_NAME)
                        .color(ink),
                    text(sheet.count.resolve(self.lang)).size(14).color(quiet),
                ]
                .width(Length::Fill),
                link(
                    self.word(sheet.close.label_key),
                    Message::Intent(sheet.close.intent.clone()),
                    t
                ),
            ]
            .align_y(Alignment::Center);
            pane = pane.push(head);
            let mut rows = column![].spacing(6);
            for r in &sheet.rows {
                let mut line = row![
                    text(r.name.resolve(self.lang))
                        .size(size::ROW_NAME)
                        .color(ink)
                        .width(Length::Fill),
                    text(r.state.resolve(self.lang)).size(14).color(quiet),
                ]
                .spacing(12)
                .align_y(Alignment::Center);
                if let Some(remove) = &r.remove {
                    line = line.push(link(
                        self.word(remove.label_key),
                        Message::Intent(remove.intent.clone()),
                        t,
                    ));
                }
                rows = rows.push(line);
            }
            pane = pane.push(soft(scrollable(rows).height(Length::Fill)));
            if let Some(ask) = &sheet.ask {
                pane = pane.push(text(ask.words.resolve(self.lang)).size(15).color(ink));
                pane = pane.push(row![
                    flat(
                        text(self.word(StrId::GroupYes))
                            .size(15)
                            .color(colour(t.failed_mark_on_panel)),
                        Message::Intent(ask.yes.intent.clone()),
                    )
                    .padding([4, 8]),
                    link(
                        self.word(StrId::GroupNo),
                        Message::Intent(ask.no.intent.clone()),
                        t
                    ),
                ]);
            } else {
                // The admin's Add members (1307 G2), above Leave.
                if let Some(add) = &sheet.add {
                    pane = pane.push(link(
                        self.word(add.label_key),
                        Message::Intent(add.intent.clone()),
                        t,
                    ));
                }
                // Start again with the same people (1307 pass four), in a
                // group the reader left or that ended.
                if let Some(again) = &sheet.again {
                    pane = pane.push(link(
                        self.word(again.label_key),
                        Message::Intent(again.intent.clone()),
                        t,
                    ));
                }
                if let Some(leave) = &sheet.leave {
                    pane = pane.push(flat(
                        text(self.word(leave.label_key))
                            .size(15)
                            .color(colour(t.failed_mark_on_panel)),
                        Message::Intent(leave.intent.clone()),
                    ));
                }
                // The admin's Delete group (pass four), under Leave.
                if let Some(delete) = &sheet.delete {
                    pane = pane.push(flat(
                        text(self.word(delete.label_key))
                            .size(15)
                            .color(colour(t.failed_mark_on_panel)),
                        Message::Intent(delete.intent.clone()),
                    ));
                }
            }
        }
        container(pane)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_: &Theme| container::Style {
                background: Some(Background::Color(colour(t.background))),
                ..container::Style::default()
            })
            .into()
    }

    /// An invitation's card (1307): who invited to what, the trust
    /// sentence, and Join or where the reader stands.
    fn invite_card<'a>(
        &self,
        invite: &'a InviteNode,
        in_bubble: bool,
        t: Colours,
    ) -> Element<'a, Message> {
        let ink = colour(if in_bubble {
            t.bubble_theirs_text
        } else {
            t.text
        });
        let quiet = colour(if in_bubble {
            t.bubble_theirs_meta
        } else {
            t.text_secondary
        });
        let mut card = column![
            text(invite.title.resolve(self.lang))
                .size(size::BODY)
                .color(ink),
            text(invite.trust.resolve(self.lang)).size(13).color(quiet),
        ]
        .spacing(4);
        if let Some(join) = &invite.join {
            card = card.push(filled(
                self.word(join.label_key),
                Some(Message::Intent(join.intent.clone())),
                t,
            ));
        } else if let Some(state) = &invite.state {
            card = card.push(text(state.resolve(self.lang)).size(13).color(quiet));
        }
        card.into()
    }

    /// The chat's top bar: the peer's avatar, the name, the bar's actions.
    fn chat_bar<'a>(&'a self, open: &'a Chat, t: Colours) -> Element<'a, Message> {
        let mut line = row![].spacing(12).align_y(Alignment::Center);
        let avatar_of = match &open.bar.lead {
            Lead::Back { back, avatar } => {
                // Stacked, the chat's back leads to the list; beside the
                // list there is nothing to go back to.
                if self.class() == LayoutClass::Compact {
                    line = line.push(self.back(back, t));
                }
                avatar
            }
            Lead::Me { avatar, .. } => avatar,
        };
        line = line.push(
            row![
                avatar(avatar_of, size::BAR_AVATAR, t),
                self.bar_title(&open.bar.title, t),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        );
        for action in open.bar.actions.iter().filter(|a| a.enabled) {
            line = line.push(link(
                self.word(action.label_key),
                Message::Intent(action.intent.clone()),
                t,
            ));
        }
        container(line)
            .height(size::BAR)
            .padding([0, 12])
            .align_y(Alignment::Center)
            .width(Length::Fill)
            .style(move |_: &Theme| container::Style {
                background: Some(Background::Color(colour(t.bar))),
                ..container::Style::default()
            })
            .into()
    }

    /// The chat bar's title: the name, and under it, small, the sentence of
    /// a name two peers share (1452), which the scene joins to the name.
    fn bar_title<'a>(&self, title: &Text, t: Colours) -> Element<'a, Message> {
        let line = |words: String| {
            text(words)
                .size(size::BAR_NAME)
                .color(colour(t.text))
                .width(Length::Fill)
                .wrapping(text::Wrapping::None)
        };
        match title {
            Text::Join(parts) if parts.len() == 2 => column![
                line(parts[0].resolve(self.lang)),
                text(parts[1].resolve(self.lang))
                    .size(size::ROW_TIME)
                    .color(colour(t.text_secondary))
                    .wrapping(text::Wrapping::None),
            ]
            .width(Length::Fill)
            .clip(true)
            .into(),
            title => text(title.resolve(self.lang))
                .size(size::BAR_NAME)
                .color(colour(t.text))
                .width(Length::Fill)
                .into(),
        }
    }

    /// A bar's back as a chevron.
    fn back<'a>(&self, back: &Action, t: Colours) -> Element<'a, Message> {
        self.icon_action(
            Icon::Back,
            self.word(back.label_key),
            Message::Intent(back.intent.clone()),
            t,
        )
    }

    /// The chat's groups: each one's day line when a day begins with it,
    /// its bubbles with the scene's gaps, and the menu a right click
    /// opened.
    fn groups<'a>(&'a self, open: &'a Chat, t: Colours) -> Element<'a, Message> {
        let width = self.chat_width();
        let picked = self.picked_ranges();
        // Only the lines on screen are built and laid out (1487): each
        // line of the chat named, the rows widget builds the ones in view.
        let (lines, items) = self.chat_lines(open, width);
        // The message each line is, for what the rows say on screen
        // (1457): a bubble's end, by the read rule.
        let keys: Vec<Option<u64>> = lines
            .iter()
            .map(|line| match *line {
                ChatLine::Bubble(g, n) | ChatLine::Note(g, n) => {
                    Some(open.groups[g].bubbles[n].bubble.key)
                }
                _ => None,
            })
            .collect();
        let peer = open.peer;
        let body = rows(items, move |i| {
            self.chat_line(open, lines[i], width, &picked, t)
        })
        .padding([0, 12])
        .anchor_end()
        .on_shown(move |shown| {
            Message::Shown(peer, shown.iter().filter_map(|&i| keys[i]).collect())
        });
        soft_picking(
            scrollable(body)
                .anchor_bottom()
                .height(Length::Fill)
                .width(Length::Fill)
                .style(move |theme: &Theme, status| {
                    let mut s = scrollable::default(theme, status);
                    s.container.background = Some(Background::Color(colour(t.background)));
                    s
                }),
            self.picking,
        )
    }

    /// A line of the chat nobody wrote (a day, a group's own line) as it is
    /// drawn, made selectable as the run `key` (1475).
    fn quiet_line<'a>(
        &self,
        words: String,
        key: RunKey,
        picked: &HashMap<RunKey, Range<usize>>,
        t: Colours,
    ) -> Element<'a, Message> {
        let ink = colour(t.text_secondary);
        selectable(
            text(words.clone()).size(size::DAY).color(ink),
            words,
            picked.get(&key).cloned(),
            Color { a: 0.3, ..ink },
            self.picking,
            move |pick| Message::PickText(key, pick),
        )
        .into()
    }

    /// The chat's lines in order, as the column of them was before 1487
    /// (a day's line, a writer's name, the gap above a bubble, the bubble,
    /// its late line and its menu, a group's own line, the gap at the
    /// foot), each with its key and its stamp: what changes its height
    /// moves the stamp, so a line measured once is not built again until
    /// it changes.
    fn chat_lines(&self, open: &Chat, width: f32) -> (Vec<ChatLine>, Vec<Item>) {
        let mut lines = Vec::new();
        let mut items = Vec::new();
        let base = {
            let mut h = std::hash::DefaultHasher::new();
            std::mem::discriminant(&self.lang).hash(&mut h);
            width.to_bits().hash(&mut h);
            h.finish()
        };
        let mut push = |line: ChatLine, key: (u8, u64), stamp: u64| {
            let mut k = std::hash::DefaultHasher::new();
            key.hash(&mut k);
            lines.push(line);
            items.push(Item {
                key: k.finish(),
                stamp: stamp ^ base,
            });
        };
        for (g, group) in open.groups.iter().enumerate() {
            if let (Some(day), Some(first)) = (&group.day, group.bubbles.first()) {
                push(
                    ChatLine::Day(g),
                    (1, first.bubble.key),
                    stamp_of(day.at.unix_secs),
                );
            }
            if group.named {
                let mut h = std::hash::DefaultHasher::new();
                group.id.hash(&mut h);
                push(ChatLine::Named(g), (2, h.finish()), 0);
            }
            for (n, node) in group.bubbles.iter().enumerate() {
                let key = node.bubble.key;
                if node.bubble.note {
                    push(ChatLine::Note(g, n), (3, key), stamp_of(&node.bubble.body));
                    continue;
                }
                let place = &node.place;
                let gap = match place.joins_before {
                    true => size::GAP_GROUPED,
                    false if place.day => 0.0,
                    false => size::GAP,
                };
                push(ChatLine::Gap(g, n), (4, key), u64::from(gap.to_bits()));
                push(ChatLine::Bubble(g, n), (5, key), bubble_stamp(node));
                if node.bubble.late {
                    push(ChatLine::Late(g, n), (6, key), 0);
                }
                if self.menu == Some(node.id) {
                    push(ChatLine::Actions(g, n), (7, key), 0);
                }
            }
        }
        push(ChatLine::Foot, (8, 0), 0);
        (lines, items)
    }

    /// Line `line` of the chat as it is drawn.
    fn chat_line<'a>(
        &'a self,
        open: &'a Chat,
        line: ChatLine,
        width: f32,
        picked: &HashMap<RunKey, Range<usize>>,
        t: Colours,
    ) -> Element<'a, Message> {
        let node = |g: usize, n: usize| &open.groups[g].bubbles[n];
        match line {
            ChatLine::Day(g) => {
                let group = &open.groups[g];
                let (Some(day), Some(first)) = (&group.day, group.bubbles.first()) else {
                    return Space::new().into();
                };
                let key = RunKey::new(first.bubble.key, Part::Day);
                container(self.quiet_line(day.words.resolve(self.lang), key, picked, t))
                    .center_x(Length::Fill)
                    .padding(iced::Padding::ZERO.top(16).bottom(8))
                    .into()
            }
            // A group's chat names the writer above a group of bubbles that
            // is not the reader's (1307).
            ChatLine::Named(g) => container(
                text(open.groups[g].sender.resolve(self.lang))
                    .size(13)
                    .color(colour(t.accent)),
            )
            .padding(iced::Padding::ZERO.top(size::GAP).left(12))
            .into(),
            // A group's own line, "Anna joined." (1307 G2): centred, as
            // the day line, no one's bubble.
            ChatLine::Note(g, n) => {
                let node = node(g, n);
                let key = RunKey::new(node.bubble.key, Part::Note);
                container(self.quiet_line(node.bubble.body.clone(), key, picked, t))
                    .center_x(Length::Fill)
                    .padding(iced::Padding::ZERO.top(8).bottom(4))
                    .into()
            }
            ChatLine::Gap(g, n) => {
                let place = &node(g, n).place;
                let gap = match place.joins_before {
                    true => size::GAP_GROUPED,
                    false if place.day => 0.0,
                    false => size::GAP,
                };
                Space::new().height(gap).into()
            }
            ChatLine::Bubble(g, n) => self.bubble(open, node(g, n), width, picked, t),
            // A line that came after newer ones (1307 pass four).
            ChatLine::Late(..) => text(self.word(StrId::GroupArrivedLate))
                .size(size::META)
                .color(colour(t.text_secondary))
                .into(),
            ChatLine::Actions(g, n) => self.actions(node(g, n), t),
            ChatLine::Foot => Space::new().height(size::GAP).into(),
        }
    }

    /// The chat's width in logical pixels, for the bubbles' widest.
    fn chat_width(&self) -> f32 {
        let beside = match self.class() {
            LayoutClass::Wide => LIST_WIDTH + RAIL,
            LayoutClass::Compact => 0.0,
        };
        (self.window.width - beside).max(200.0)
    }

    fn bubble<'a>(
        &'a self,
        open: &'a Chat,
        node: &'a BubbleNode,
        width: f32,
        picked: &HashMap<RunKey, Range<usize>>,
        t: Colours,
    ) -> Element<'a, Message> {
        let (bubble, place) = (&node.bubble, &node.place);
        let ours = bubble.ours;
        let (ground, ink, meta) = match ours {
            true => (t.bubble_ours, t.bubble_ours_text, t.bubble_ours_meta),
            false => (t.bubble_theirs, t.bubble_theirs_text, t.bubble_theirs_meta),
        };
        let (ground, ink, meta) = (colour(ground), colour(ink), colour(meta));
        let mut inside = column![].spacing(4);
        if let Some(quote) = &bubble.quote {
            let who = match quote.ours {
                Some(true) => Some(self.word(StrId::QuoteYou).to_string()),
                Some(false) => Some(loquax::ui::scene::peer_title(&open.bar).resolve(self.lang)),
                None => None,
            };
            let mut said = column![];
            if let Some(who) = who {
                said = said.push(text(who).size(13).color(meta));
            }
            // The quote selects (1475), its own run of the bubble.
            let key = RunKey::new(bubble.key, Part::Quote);
            said = said.push(selectable(
                text(quote.text.clone()).size(14).color(meta),
                quote.text.as_str(),
                picked.get(&key).cloned(),
                Color { a: 0.3, ..meta },
                self.picking,
                move |pick| Message::PickText(key, pick),
            ));
            let stripe =
                container(Space::new().width(3).height(Length::Fill)).style(move |_: &Theme| {
                    container::Style {
                        background: Some(Background::Color(meta)),
                        ..container::Style::default()
                    }
                });
            inside = inside.push(row![stripe, said].spacing(8).height(Length::Shrink));
        }
        // The media first, inside the one bubble (1268). A photo (1220):
        // its words, a press shows it whole in the system's viewer. The
        // thumbnail in the window is a later slice.
        // A picture sent as a file (1662) is its file's line here: the
        // window draws no thumbnail.
        if let Some(photo) = node.photo.as_ref().filter(|p| p.file.is_none()) {
            inside = inside.push(flat(
                text(format!("▣ {}", photo.words.resolve(self.lang)))
                    .size(size::BODY - 2.0)
                    .color(ink),
                Message::Intent(photo.open.intent.clone()),
            ));
        }
        // A file (1221): its name and size, a press saves it under the
        // home and opens it with the system's handler.
        for file in &node.files {
            inside = inside.push(flat(
                text(format!("▤ {} · {}", file.name, file.size))
                    .size(size::BODY - 2.0)
                    .color(ink),
                Message::Intent(file.open.intent.clone()),
            ));
        }
        // A voice message (1222): its length, a press plays it in the
        // system's player.
        if let Some(voice) = &node.voice {
            let line = text(format!("♪ {}", voice.words.resolve(self.lang)))
                .size(size::BODY - 2.0)
                .color(ink);
            let line: Element<'_, Message> = match &voice.toggle {
                Some(toggle) => flat(line, Message::Intent(toggle.intent.clone())).into(),
                None => line.into(),
            };
            inside = inside.push(line);
            // Its waveform (1276): the scene's bars, the recording's own
            // track where the model has one.
            if !voice.levels.is_empty() {
                inside = inside.push(wave(
                    &voice.levels,
                    voice.levels.len(),
                    false,
                    meta,
                    Length::Fixed(4.0 * voice.levels.len() as f32),
                    22.0,
                ));
            }
        }
        // Its bytes under way (1662): the words and a thin bar under the
        // media, in the bubble's ink.
        if let Some(progress) = &node.progress {
            inside = inside.push(
                text(progress.words.resolve(self.lang))
                    .size(size::BODY - 2.0)
                    .color(meta),
            );
            inside = inside.push(
                progress_bar(0.0..=100.0, f32::from(progress.percent))
                    .length(Length::Fixed(160.0))
                    .girth(4.0)
                    .style(move |_: &Theme| progress_bar::Style {
                        background: Background::Color(Color { a: 0.25, ..meta }),
                        bar: Background::Color(ink),
                        border: border::rounded(2.0),
                    }),
            );
        }
        // An invitation is a card (1307): its words, the trust sentence and
        // Join, in place of the text a stock client reads.
        if let Some(invite) = &node.invite {
            inside = inside.push(self.invite_card(invite, !ours, t));
        } else if let Some(words) = &node.text {
            // The words can be selected (1444): the selection under them
            // in the ink, faint, which shows on either bubble's ground.
            let key = RunKey::body(bubble.key);
            inside = inside.push(selectable(
                text(words.as_str()).size(size::BODY).color(ink),
                words,
                picked.get(&key).cloned(),
                Color { a: 0.3, ..ink },
                self.picking,
                move |pick| Message::PickText(key, pick),
            ));
        }
        if let (Some(fix), Some(words)) = (&bubble.fix, position_line(node, self.lang)) {
            let stale = self.seen.now.is_some_and(|now| {
                fix.fix_time()
                    .saturating_add(loquax::telemetry::STALE_AFTER_SECS)
                    < now.unix_secs
            });
            let line_ink = match stale {
                true => meta,
                false => ink,
            };
            // A press opens the map; a drag or a double click selects
            // the line (1475).
            let key = RunKey::new(bubble.key, Part::Position);
            let line = selectable(
                text(words.clone()).size(size::BODY - 2.0).color(line_ink),
                words,
                picked.get(&key).cloned(),
                Color { a: 0.3, ..line_ink },
                self.picking,
                move |pick| Message::PickText(key, pick),
            )
            .in_button();
            inside = inside.push(flat(
                line,
                Message::Intent(Intent::ShowMap {
                    peer: Some(open.peer),
                }),
            ));
        }
        if place.meta && (bubble.at.is_some() || ours) {
            let mut line = row![].spacing(4).align_y(Alignment::Center);
            if let Some(at) = bubble.at {
                line = line.push(text(at.hh_mm()).size(size::META).color(meta));
            }
            if let Some(mark) = &node.mark {
                let failed = (colour(t.failed_ground), colour(t.failed_mark));
                let ink = colour(chat::bubble_ink(mark.mark, &t));
                let face = drawing(mark_icon(mark.mark), ink, failed, 16.0);
                // A failed one sends again on a press (1232).
                line = match mark.activate.is_some() {
                    true => line.push(flat(
                        face,
                        Message::Intent(Intent::Activate(scene::NodeId::Mark(bubble.key))),
                    )),
                    false => line.push(face),
                };
            }
            inside = inside.push(container(line).align_right(Length::Shrink));
        }
        let big = size::CORNER;
        let small = size::CORNER_GROUPED;
        let top = if place.joins_before { small } else { big };
        let bottom = if place.joins_after { small } else { big };
        // The inner side is the sender's: the right for ours.
        let radius = match ours {
            true => border::Radius {
                top_left: big,
                top_right: top,
                bottom_right: bottom,
                bottom_left: big,
            },
            false => border::Radius {
                top_left: top,
                top_right: big,
                bottom_right: big,
                bottom_left: bottom,
            },
        };
        let shape = container(inside)
            .padding([8, 12])
            .max_width(width * size::BUBBLE_PART)
            .style(move |_: &Theme| container::Style {
                background: Some(Background::Color(ground)),
                border: Border {
                    radius,
                    ..Border::default()
                },
                ..container::Style::default()
            });
        let pressed =
            mouse_area(shape).on_right_press(Message::Intent(Intent::Menu(Some(node.id))));
        let mut stack = column![pressed];
        if !bubble.chips.is_empty() {
            let pills = bubble
                .chips
                .iter()
                .fold(row![].spacing(4), |pills, chip| pills.push(pill(chip, t)));
            stack = stack.push(container(pills).padding([0, 8]));
        }
        let stack = match ours {
            true => stack.align_x(Alignment::End),
            false => stack.align_x(Alignment::Start),
        };
        let side = container(stack).width(Length::Fill);
        match ours {
            true => side.align_right(Length::Fill).into(),
            false => side.align_left(Length::Fill).into(),
        }
    }

    /// The menu under a bubble a right click opened: the scene's actions
    /// of the bubble, a reaction as the quick row, a delete asked once
    /// before it deletes, and a close.
    /// A row's menu (1371): the scene's verbs on the conversation (mark
    /// unread, pin, mute, archive, forget), then the contact's the phone
    /// has on its contact screen (block, verify, rename). Forget asks
    /// once; rename opens its field.
    fn row_actions<'a>(&'a self, r: &'a Row, t: Colours) -> Element<'a, Message> {
        let scene::NodeId::Row(peer) = r.id else {
            return Space::new().into();
        };
        let target = loquax::ui::intent::Target::Peer(peer);
        let contact = [
            Intent::Block {
                peer: target,
                on: Switch::On,
            },
            Intent::Verify {
                peer: target,
                on: Switch::Toggle,
            },
            Intent::Rename {
                peer: target,
                name: None,
            },
        ];
        let mut verbs = column![].spacing(2);
        let intents = r
            .item
            .menu
            .iter()
            .filter(|a| a.enabled)
            .map(|a| a.intent.clone())
            .chain(contact);
        for intent in intents {
            let label = self.word(intent.label());
            let line = match intent {
                Intent::Forget {
                    peer, everywhere, ..
                } if self.deleting == Some(r.id) => flat(
                    text(format!("{label}, sure?"))
                        .size(15)
                        .color(colour(t.failed_mark_on_panel)),
                    Message::Intent(Intent::Forget {
                        peer,
                        everywhere,
                        confirmed: true,
                    }),
                )
                .padding([4, 8])
                .into(),
                intent => link(label, Message::Intent(intent), t),
            };
            verbs = verbs.push(line);
        }
        if let Some((_, name)) = self.renaming.as_ref().filter(|(p, _)| *p == peer) {
            let field = chrome::search_field(
                self.word(StrId::Rename),
                name,
                size::COMPOSE_CORNER,
                palette(t),
            )
            .on_input(Message::Renaming)
            .on_submit(Message::Intent(Intent::Rename {
                peer: target,
                name: Some(name.clone()),
            }));
            verbs = verbs.push(field);
        }
        verbs = verbs.push(link(
            self.word(StrId::Close),
            Message::Intent(Intent::Menu(None)),
            t,
        ));
        container(verbs)
            .padding(8)
            .width(Length::Fill)
            .style(move |_: &Theme| container::Style {
                background: Some(Background::Color(colour(t.panel))),
                border: Border {
                    radius: border::radius(12),
                    width: 1.0,
                    color: colour(t.bar),
                },
                ..container::Style::default()
            })
            .into()
    }

    fn actions<'a>(&'a self, node: &'a BubbleNode, t: Colours) -> Element<'a, Message> {
        let mut reactions = row![].spacing(6);
        let mut actions = row![].spacing(4);
        for action in node.actions.iter().filter(|a| a.enabled) {
            match &action.intent {
                Intent::React { to, target, .. } => {
                    for quick in SIGNAL_QUICK {
                        let react = Intent::React {
                            to: *to,
                            target: *target,
                            content: Some(quick.to_string()),
                        };
                        reactions = reactions
                            .push(flat(text(quick).size(20), Message::Intent(react)).padding(4));
                    }
                }
                Intent::Delete { messages, .. } => {
                    let asked = self.deleting == Some(node.id);
                    let (words, confirmed) = match asked {
                        true => (format!("{}, sure?", self.word(action.label_key)), true),
                        false => (self.word(action.label_key).to_string(), false),
                    };
                    let delete = Intent::Delete {
                        messages: messages.clone(),
                        confirmed,
                    };
                    actions = actions.push(
                        flat(
                            text(words).size(15).color(colour(t.failed_mark_on_panel)),
                            Message::Intent(delete),
                        )
                        .padding([4, 8]),
                    );
                }
                intent => {
                    actions = actions.push(link(
                        self.word(action.label_key),
                        Message::Intent(intent.clone()),
                        t,
                    ));
                }
            }
        }
        // Info (1457): the phone's dialog, the core's rows over the chat.
        actions = actions.push(link(
            self.word(StrId::ActionInfo),
            Message::Intent(Intent::Details),
            t,
        ));
        // Forward (1371): the phone's picker of conversations, here the
        // list's rows under the menu.
        actions = actions.push(link(
            self.word(StrId::ActionForward),
            Message::ForwardPick(node.id),
            t,
        ));
        actions = actions.push(link(
            self.word(StrId::Close),
            Message::Intent(Intent::Menu(None)),
            t,
        ));
        let mut panel = column![reactions, actions].spacing(4);
        if let (Some(from), scene::NodeId::Bubble(key), true) = (
            self.scene.chat.as_ref().map(|c| c.peer),
            node.id,
            self.forwarding == Some(node.id),
        ) {
            for r in self.listed() {
                if let scene::NodeId::Row(to) = r.id {
                    panel = panel.push(link(
                        &r.row.name,
                        Message::Intent(Intent::Forward { from, key, to }),
                        t,
                    ));
                }
            }
        }
        let panel = container(panel)
            .padding(8)
            .style(move |_: &Theme| container::Style {
                background: Some(Background::Color(colour(t.panel))),
                border: Border {
                    radius: border::radius(12),
                    width: 1.0,
                    color: colour(t.bar),
                },
                ..container::Style::default()
            });
        let side = container(panel).width(Length::Fill).padding([4, 0]);
        match node.bubble.ours {
            true => side.align_right(Length::Fill).into(),
            false => side.align_left(Length::Fill).into(),
        }
    }

    /// The compose bar: the reply being written, the emoji row when open,
    /// the pill with its emoji button and field, the round send button
    /// raising the scene's send.
    fn compose_bar<'a>(&'a self, open: &'a Chat, t: Colours) -> Element<'a, Message> {
        if let Some(recording) = &open.compose.recording {
            return self.recording_bar(recording, t);
        }
        let mut bar = column![].spacing(6).padding([8, 12]);
        if let Some((_, quoted)) = &self.reply {
            bar = bar.push(
                row![
                    text(format!("Reply to: {quoted}"))
                        .size(14)
                        .color(colour(t.text_secondary))
                        .width(Length::Fill),
                    link(
                        self.word(Intent::CancelReply.label()),
                        Message::Intent(Intent::CancelReply),
                        t
                    ),
                ]
                .align_y(Alignment::Center),
            );
        }
        // The plus's sheet or the position sheet (1270), over the bar as
        // the phone's bottom sheet: its title, a row a line, Dismiss.
        if let Some(sheet) = &self.scene.sheet {
            let mut rows = column![row![
                text(sheet.title.resolve(self.lang))
                    .size(15)
                    .color(colour(t.text))
                    .width(Length::Fill),
                link(
                    self.word(Intent::CloseSheet.label()),
                    Message::Intent(Intent::CloseSheet),
                    t
                ),
            ]
            .align_y(Alignment::Center)]
            .spacing(6);
            if let Some(whereabouts) = &self.scene.whereabouts {
                rows = rows.push(
                    text(whereabouts.words.resolve(self.lang))
                        .size(14)
                        .color(colour(t.text_secondary)),
                );
            }
            for action in sheet.actions.iter().filter(|a| a.enabled) {
                rows = rows.push(link(
                    self.word(action.label_key),
                    Message::Intent(action.intent.clone()),
                    t,
                ));
            }
            // The position by hand (1371): latitude and longitude typed or
            // pasted, Enter sets them.
            if let Some(typed) = &self.position {
                let field = chrome::search_field(
                    self.word(StrId::PositionHint),
                    typed,
                    size::COMPOSE_CORNER,
                    palette(t),
                )
                .id(POSITION_FIELD)
                .on_input(Message::Position)
                .on_submit(Message::Intent(Intent::EnterPosition {
                    text: Some(typed.clone()),
                }));
                rows = rows.push(field);
            }
            bar = bar.push(container(rows).padding([8, 12]).width(Length::Fill).style(
                move |_: &Theme| container::Style {
                    background: Some(Background::Color(colour(t.bar))),
                    border: border::rounded(size::COMPOSE_CORNER),
                    ..container::Style::default()
                },
            ));
        }
        if self.emoji {
            let picks = SIGNAL_QUICK.iter().fold(row![].spacing(6), |picks, e| {
                picks.push(
                    flat(
                        text(*e).size(22),
                        Message::Intent(Intent::Insert(e.to_string())),
                    )
                    .padding(4),
                )
            });
            bar = bar.push(picks);
        }
        let face = flat(
            drawing(
                Icon::Face,
                colour(t.text_secondary),
                (Color::TRANSPARENT, Color::TRANSPARENT),
                28.0,
            ),
            Message::Intent(Intent::Emoji),
        )
        .padding(6);
        // The scene's buttons in the field's right edge (1270): those the
        // window can do, the microphone where a recorder is found; no
        // camera and no position, which a desktop does not read.
        let tail = open
            .compose
            .buttons
            .iter()
            .filter(|b| b.button.inside() && b.button != BarButton::Emoji)
            .fold(row![].align_y(Alignment::Center), |tail, b| {
                let icon = match b.button {
                    BarButton::Locate => Icon::Locate,
                    BarButton::Camera => Icon::Camera,
                    _ => Icon::Mic,
                };
                let face = drawing(
                    icon,
                    colour(t.text_secondary),
                    (Color::TRANSPARENT, Color::TRANSPARENT),
                    28.0,
                );
                // The microphone is held, not clicked (1276): the button
                // goes down with the mouse, the window follows the
                // pointer until it comes up.
                match b.button {
                    BarButton::Microphone => tail.push(
                        mouse_area(container(face).padding(6))
                            .on_press(Message::RecPress)
                            .interaction(mouse::Interaction::Pointer),
                    ),
                    _ => tail.push(flat(face, Message::Intent(b.action.intent.clone())).padding(6)),
                }
            });
        // The round one: the plus while there is nothing to send, Send
        // once there is (the scene's rule); the plus opens the sheet with
        // the desktop chooser's Gallery and File (1341).
        let plus = open
            .compose
            .buttons
            .iter()
            .find(|b| b.button == BarButton::Plus)
            .map(|b| b.action.intent.clone());
        let send = Message::Intent(open.compose.send.intent.clone());
        let enter = send.clone();
        let editor = text_editor(&self.compose)
            .placeholder(format!(
                "{} {}",
                open.compose.hint.resolve(self.lang),
                loquax::ui::scene::peer_title(&open.bar).resolve(self.lang)
            ))
            .size(size::COMPOSE_TEXT)
            .height(Length::Shrink)
            .padding(iced::Padding::from([COMPOSE_PAD_Y, COMPOSE_PAD_X]))
            .on_action(Message::Compose)
            .key_binding(move |press| {
                let is_enter = matches!(
                    press.key,
                    iced::keyboard::Key::Named(iced::keyboard::key::Named::Enter)
                );
                match (is_enter, press.modifiers.shift()) {
                    (true, false) => Some(text_editor::Binding::Custom(enter.clone())),
                    _ => text_editor::Binding::from_key_press(press),
                }
            })
            .style(move |_: &Theme, _| text_editor::Style {
                background: Background::Color(Color::TRANSPARENT),
                border: Border::default(),
                placeholder: colour(t.text_secondary),
                value: colour(t.text),
                selection: colour(t.selected_row),
            });
        // A middle click on the field pastes the PRIMARY selection (1444).
        // Where it clicked (1475): the pointer's place over the field.
        let editor = mouse_area(editor)
            .on_middle_press(Message::PastePrimary)
            .on_move(Message::ComposePointer);
        let pill = container(row![face, editor, tail].align_y(Alignment::Center))
            .width(Length::Fill)
            .padding([0, 6])
            .style(move |_: &Theme| container::Style {
                background: Some(Background::Color(colour(t.bar))),
                border: border::rounded(size::COMPOSE_CORNER),
                ..container::Style::default()
            });
        let (round, press) = match plus {
            Some(intent) => (Icon::Plus, Message::Intent(intent)),
            None => (Icon::Send, send),
        };
        let send = button(
            container(drawing(
                round,
                colour(t.on_send),
                (Color::TRANSPARENT, Color::TRANSPARENT),
                28.0,
            ))
            .center(size::SEND),
        )
        .on_press(press)
        .padding(0)
        .width(size::SEND)
        .height(size::SEND)
        .style(move |_: &Theme, _| button::Style {
            background: Some(Background::Color(colour(t.send))),
            border: border::rounded(size::SEND / 2.0),
            ..button::Style::default()
        });
        container(bar.push(row![pill, send].spacing(8).align_y(Alignment::End)))
            .width(Length::Fill)
            .style(move |_: &Theme| container::Style {
                background: Some(Background::Color(colour(t.background))),
                ..container::Style::default()
            })
            .into()
    }

    /// The recording bar in the compose bar's place (1276,
    /// `docs/src/concepts/loquax-voice-recording.md`): the red dot,
    /// pulsing every half second while it records and is heard, hollow
    /// once it stands; `REC 00:07`, amber in the last 30 s; the waveform;
    /// while held the cancel hint and the grown red microphone with the
    /// lock chip above it, once locked the bin, the pause and a big Send,
    /// paused the play, the bin, resume; under it the line the scene says.
    fn recording_bar<'a>(&'a self, bar: &'a RecordingBar, t: Colours) -> Element<'a, Message> {
        let red = colour(t.rec_red);
        let none = (Color::TRANSPARENT, Color::TRANSPARENT);
        let running = bar.phase.running();
        // The pulse as frames on the timer's clock, so a picture of one
        // moment is always the same picture.
        let bright = !bar.pulsing || (bar.timer.ms / 500).is_multiple_of(2);
        let dot_ink = match bright {
            true => red,
            false => Color { a: 0.35, ..red },
        };
        let dot: Element<'_, Message> = match running {
            true => container(Space::new().width(12).height(12))
                .style(move |_: &Theme| container::Style {
                    background: Some(Background::Color(dot_ink)),
                    border: border::rounded(6),
                    ..container::Style::default()
                })
                .into(),
            false => container(Space::new().width(12).height(12))
                .style(move |_: &Theme| container::Style {
                    border: border::rounded(6).width(2).color(dot_ink),
                    ..container::Style::default()
                })
                .into(),
        };
        let timer_ink = match bar.timer.amber {
            true => colour(t.rec_amber),
            false => red,
        };
        let timer = text(bar.timer.words.resolve(self.lang))
            .size(20)
            .color(timer_ink)
            .font(iced::Font {
                weight: iced::font::Weight::Bold,
                ..iced::Font::DEFAULT
            });
        let wave_ink = match running {
            true => colour(t.text),
            false => colour(t.text_secondary),
        };
        let mut inside = row![
            dot,
            timer,
            wave(
                &bar.wave.levels,
                loquax::ui::recording::LIVE_WINDOWS,
                bar.wave.moving,
                wave_ink,
                Length::Fill,
                28.0,
            ),
        ]
        .spacing(10)
        .align_y(Alignment::Center);
        if let Some(hint) = &bar.cancel {
            let ink = match hint.armed {
                true => red,
                false => colour(t.text_secondary),
            };
            // The hint follows the pointer left, up to the distance.
            let shift = f32::from(hint.progress) * 0.4;
            inside = inside.push(
                container(
                    text(format!("‹ {}", hint.words.resolve(self.lang)))
                        .size(15)
                        .color(ink),
                )
                .padding(iced::Padding {
                    right: shift,
                    ..iced::Padding::ZERO
                }),
            );
        }
        let icon_of = |verb: Verb| match verb {
            Verb::Discard => Icon::Bin,
            Verb::Pause => Icon::Pause,
            Verb::Play => Icon::Play,
            Verb::StopPlay => Icon::Stop,
            _ => Icon::Mic,
        };
        let mut send = None;
        for b in &bar.buttons {
            let press = Message::Intent(b.action.intent.clone());
            match b.verb {
                Verb::Send => send = Some(press),
                // Resume is a recorder's red dot: it records again; its
                // words are the tooltip's.
                Verb::Resume => {
                    let dot =
                        container(Space::new().width(16).height(16)).style(move |_: &Theme| {
                            container::Style {
                                background: Some(Background::Color(red)),
                                border: border::rounded(8),
                                ..container::Style::default()
                            }
                        });
                    inside = inside.push(tooltip(
                        flat(container(dot).center(26), press).padding(4),
                        text(self.word(StrId::RecResume)).size(13),
                        tooltip::Position::Top,
                    ));
                }
                verb => {
                    let ink = match verb {
                        Verb::Discard => red,
                        _ => colour(t.text),
                    };
                    inside = inside.push(tooltip(
                        flat(drawing(icon_of(verb), ink, none, 26.0), press).padding(4),
                        text(self.word(verb.label())).size(13),
                        tooltip::Position::Top,
                    ));
                }
            }
        }
        let pill = container(inside)
            .width(Length::Fill)
            .height(size::SEND)
            .padding([0, 14])
            .align_y(Alignment::Center)
            .style(move |_: &Theme| container::Style {
                background: Some(Background::Color(colour(t.bar))),
                border: border::rounded(size::COMPOSE_CORNER),
                ..container::Style::default()
            });
        // The round button: the grown red microphone under the held
        // pointer, a big Send once there is something to send.
        let side = size::SEND + 8.0;
        let (round, ground, press) = match (&bar.held, send) {
            (Some(_), _) => (Icon::Mic, red, None),
            (None, Some(press)) => (Icon::Send, colour(t.send), Some(press)),
            (None, None) => (Icon::Send, colour(t.send), None),
        };
        let mut round =
            button(container(drawing(round, colour(t.on_send), none, 30.0)).center(side))
                .padding(0)
                .width(side)
                .height(side)
                .style(move |_: &Theme, _| button::Style {
                    background: Some(Background::Color(ground)),
                    border: border::rounded(side / 2.0),
                    ..button::Style::default()
                });
        if let Some(press) = press {
            round = round.on_press(press);
        }
        // Above the button, the lock chip: open with its chevron while
        // held, rising as the pointer goes up; closed once locked.
        let mut lock_row = row![Space::new().width(Length::Fill)];
        if let Some(lock) = &bar.lock {
            let ink = match lock.closed {
                true => red,
                false => colour(t.text_secondary),
            };
            let chip = container(drawing(
                Icon::Lock {
                    closed: lock.closed,
                },
                ink,
                none,
                26.0,
            ))
            .padding(8)
            .style(move |_: &Theme| container::Style {
                background: Some(Background::Color(colour(t.bar))),
                border: border::rounded(14),
                ..container::Style::default()
            });
            let rise = f32::from(lock.progress) * 0.16;
            let chip: Element<'_, Message> = match &lock.action {
                // A press while held is not to be had with one mouse
                // button: the release over the chip locks.
                Some(_) => mouse_area(chip)
                    .on_enter(Message::RecOverLock(true))
                    .on_exit(Message::RecOverLock(false))
                    .into(),
                None => chip.into(),
            };
            lock_row = lock_row.push(container(chip).width(side).center_x(side).padding(
                iced::Padding {
                    bottom: rise,
                    ..iced::Padding::ZERO
                },
            ));
        }
        let mut column = column![
            lock_row,
            row![pill, round].spacing(8).align_y(Alignment::Center)
        ]
        .spacing(6)
        .padding([8, 12]);
        if let Some(notice) = &bar.notice {
            let mut line = row![text(notice.words.resolve(self.lang))
                .size(15)
                .color(colour(t.rec_amber))
                .width(Length::Fill)]
            .spacing(8)
            .align_y(Alignment::Center);
            if let Some(action) = &notice.action {
                line = line.push(link(
                    self.word(StrId::RecAllow),
                    Message::Intent(action.intent.clone()),
                    t,
                ));
            }
            column = column.push(container(line).padding([0, 14]));
        }
        container(column)
            .width(Length::Fill)
            .style(move |_: &Theme| container::Style {
                background: Some(Background::Color(colour(t.background))),
                ..container::Style::default()
            })
            .into()
    }
}

/// One row of the list: a click is a hit on its node, which the scene
/// turns into its opening ([`scene::Scene::action`]).
fn list_row<'a>(
    r: &'a Row,
    now: Option<WallTime>,
    lang: Lang,
    t: Colours,
    picked: &HashMap<RunKey, Range<usize>>,
    picking: bool,
) -> Element<'a, Message> {
    let open = r.selected;
    let content = &r.row;
    let unread = content.unread > 0;
    let when = match (content.at, now) {
        (Some(at), Some(now)) => scene::list_time(&at, &now).resolve(lang),
        _ => String::new(),
    };
    let time_ink = match unread {
        true => colour(t.accent),
        false => colour(t.text_secondary),
    };
    let side = column![text(when).size(size::ROW_TIME).color(time_ink)]
        .align_x(Alignment::End)
        .spacing(8);
    // Under the time: the map pin (1224), left of the count or the mark.
    // A press on it is a hit on its own node, the map on the peer.
    let mut under = row![].spacing(6).align_y(Alignment::Center);
    if let Some(pin) = &r.position {
        let failed = (colour(t.list_failed_ground), colour(t.list_failed_mark));
        under = under.push(flat(
            drawing(Icon::Map, colour(pin.tint.colour(&t)), failed, 18.0),
            Message::Intent(Intent::Activate(pin.id)),
        ));
    }
    if unread {
        under = under.push(
            chrome::badge(
                content.unread,
                size::BADGE_TEXT,
                size::BADGE_CORNER,
                palette(t),
            )
            .height(size::BADGE)
            .padding([0, 7])
            .center_y(size::BADGE),
        );
    } else if let Some(mark) = content.mark {
        let failed = (colour(t.list_failed_ground), colour(t.list_failed_mark));
        under = under.push(drawing(
            mark_icon(mark),
            colour(chat::list_ink(mark, &t)),
            failed,
            18.0,
        ));
    }
    let side = side.push(under);
    // A report's line is the scene's words after the pin, as the phone
    // sets it; a text of ours is said as ours; a message typed and not
    // sent stands in for it, in italics (1219). The core words it, which
    // is also what a selection of it copies (1475).
    let last = preview_line(r, lang);
    let style = match r.draft {
        Some(_) => iced::font::Style::Italic,
        None => iced::font::Style::Normal,
    };
    let words = column![
        text(content.name.clone())
            .size(size::ROW_NAME)
            .color(colour(t.text))
            .wrapping(text::Wrapping::None),
        // The line selects (1475): a press opens the chat as before, a
        // drag or a double click selects.
        {
            let key = RunKey::new(peer_key(&content.peer), Part::Preview);
            let ink = colour(t.text_secondary);
            selectable(
                text(last.clone())
                    .size(size::ROW_LAST)
                    .color(ink)
                    .font(iced::Font {
                        style,
                        ..iced::Font::DEFAULT
                    })
                    .wrapping(text::Wrapping::None),
                last,
                picked.get(&key).cloned(),
                Color { a: 0.3, ..ink },
                picking,
                move |pick| Message::PickText(key, pick),
            )
            .in_button()
        },
    ]
    // Two peers of one name (1452): a third line says so, quiet, whole;
    // beside the name it would be cut mid-word at the list's width.
    .extend(r.same_name.as_ref().map(|same| {
        text(same.resolve(lang))
            .size(size::ROW_TIME)
            .color(colour(t.text_secondary))
            .font(iced::Font {
                style: iced::font::Style::Italic,
                ..iced::Font::DEFAULT
            })
            .wrapping(text::Wrapping::None)
            .into()
    }))
    .spacing(2)
    .width(Length::Fill)
    .clip(true);
    let line = row![avatar(&r.avatar, size::ROW_AVATAR, t), words, side]
        .spacing(size::ROW_GAP)
        .align_y(Alignment::Center);
    chrome::list_item(
        line,
        open,
        Message::Intent(Intent::Activate(r.id)),
        palette(t),
    )
}

/// A reaction under a bubble: its emoji, a count above one, ours in our
/// bubble's colour, a ring in the screen's colour lifting it off.
fn pill<'a>(chip: &'a chat::Chip, t: Colours) -> Element<'a, Message> {
    let (ground, ink) = match chip.ours {
        true => (t.bubble_ours, t.bubble_ours_text),
        false => (t.bubble_theirs, t.bubble_theirs_text),
    };
    let words = match chip.count > 1 {
        true => format!("{} {}", chip.content, chip.count),
        false => chip.content.clone(),
    };
    container(text(words).size(size::PILL).color(colour(ink)))
        .padding([2, 8])
        .style(move |_: &Theme| container::Style {
            background: Some(Background::Color(colour(ground))),
            border: Border {
                radius: border::radius(size::PILL_CORNER),
                width: 2.0,
                color: colour(t.background),
            },
            ..container::Style::default()
        })
        .into()
}
