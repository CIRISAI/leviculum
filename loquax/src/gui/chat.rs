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
    button, column, container, mouse_area, row, scrollable, text, text_editor, toggler, tooltip,
    Space,
};
use iced::{border, mouse, Alignment, Background, Border, Color, Element, Length, Theme};
use leviculum_ui_iced::chrome;
pub use leviculum_ui_iced::chrome::RAIL;
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
    self, Action, BubbleNode, Chat, Group, LayoutClass, Lead, RecordingBar, Row, Tab, TabId,
};
use loquax::ui::strings::{Arg, Lang, StrId, Text};
use loquax::when::{list_time, WallTime};

use super::{App, Message};

/// The list's width, in logical pixels.
pub const LIST_WIDTH: f32 = 340.0;

/// The search field's id: the top bar's search focuses it.
pub const SEARCH_FIELD: &str = "search";

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
    /// link's dot, the title, the bar's actions; then the window's own
    /// theme toggle.
    fn top_bar<'a>(&'a self, t: Colours) -> Element<'a, Message> {
        let top = &self.scene.top;
        let lead = match &top.lead {
            Lead::Me { avatar, link } => Some(avatar_with_dot(avatar, size::BAR_AVATAR, *link, t)),
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
        actions.push(self.icon_action(
            Icon::Theme,
            self.word(Intent::Theme.label()),
            Message::Intent(Intent::Theme),
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
        };
        let items = tabs
            .items
            .iter()
            .map(|tab| self.tab(tab, tab.id == active, t));
        let rail = self.class() == LayoutClass::Wide;
        Some(chrome::tabs(items, rail, palette(t)))
    }

    /// The settings page (1233): a bar with its back, and the row the
    /// phone's Privacy page has first, "Send read receipts" with its line
    /// and the switch as the model holds it.
    pub(super) fn settings_pane(&self) -> Element<'_, Message> {
        let t = self.tokens();
        let bar = row![
            self.icon_action(
                Icon::Back,
                self.word(StrId::Back),
                Message::Intent(Intent::Back),
                t
            ),
            text(self.word(StrId::Settings))
                .size(22)
                .color(colour(t.text))
                .width(Length::Fill),
        ]
        .spacing(12)
        .align_y(Alignment::Center);
        let words = column![
            text(self.word(StrId::ReadReceipts))
                .size(16)
                .color(colour(t.text)),
            text(self.word(StrId::ReadReceiptsText))
                .size(14)
                .color(colour(t.text_secondary)),
        ]
        .spacing(2)
        .width(Length::Fill);
        let switch = toggler(self.seen.receipts)
            .on_toggle(|on| Message::Intent(Intent::ReadReceipts { on: Switch::of(on) }))
            // On in the accent, as the phone's switch; off as iced draws it.
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
        let receipts = row![words, switch]
            .spacing(12)
            .align_y(Alignment::Center)
            .padding([12, 16]);
        container(
            column![
                container(bar)
                    .height(size::BAR)
                    .padding([0, 12])
                    .align_y(Alignment::Center),
                receipts,
                self.maps_rows(t),
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
        let rows = self.listed().into_iter().fold(column![], |rows, r| {
            rows.push(list_row(r, self.seen.now, self.lang, t))
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
            false => scrollable(rows).height(Length::Fill).into(),
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
        let mut pane = column![bar, messages];
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
            pane = pane.push(scrollable(rows).height(Length::Fill));
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
            pane = pane.push(scrollable(rows).height(Length::Fill));
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
                text(open.bar.title.resolve(self.lang))
                    .size(size::BAR_NAME)
                    .color(colour(t.text))
                    .width(Length::Fill),
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
        let mut list = column![].padding([0, 12]);
        for group in &open.groups {
            list = self.group(list, open, group, width, t);
        }
        list = list.push(Space::new().height(size::GAP));
        scrollable(list)
            .anchor_bottom()
            .height(Length::Fill)
            .width(Length::Fill)
            .style(move |theme: &Theme, status| {
                let mut s = scrollable::default(theme, status);
                s.container.background = Some(Background::Color(colour(t.background)));
                s
            })
            .into()
    }

    fn group<'a>(
        &'a self,
        mut list: iced::widget::Column<'a, Message>,
        open: &'a Chat,
        group: &'a Group,
        width: f32,
        t: Colours,
    ) -> iced::widget::Column<'a, Message> {
        if let Some(day) = &group.day {
            list = list.push(
                container(
                    text(day.words.resolve(self.lang))
                        .size(size::DAY)
                        .color(colour(t.text_secondary)),
                )
                .center_x(Length::Fill)
                .padding(iced::Padding::ZERO.top(16).bottom(8)),
            );
        }
        // A group's chat names the writer above a group of bubbles that
        // is not the reader's (1307).
        if group.named {
            list = list.push(
                container(
                    text(group.sender.resolve(self.lang))
                        .size(13)
                        .color(colour(t.accent)),
                )
                .padding(iced::Padding::ZERO.top(size::GAP).left(12)),
            );
        }
        for node in &group.bubbles {
            // A group's own line, "Anna joined." (1307 G2): centred, as
            // the day line, no one's bubble.
            if node.bubble.note {
                list = list.push(
                    container(
                        text(node.bubble.body.clone())
                            .size(size::DAY)
                            .color(colour(t.text_secondary)),
                    )
                    .center_x(Length::Fill)
                    .padding(iced::Padding::ZERO.top(8).bottom(4)),
                );
                continue;
            }
            let place = &node.place;
            let gap = match place.joins_before {
                true => size::GAP_GROUPED,
                false if place.day => 0.0,
                false => size::GAP,
            };
            list = list.push(Space::new().height(gap));
            list = list.push(self.bubble(open, node, width, t));
            // A line that came after newer ones (1307 pass four).
            if node.bubble.late {
                list = list.push(
                    text(self.word(StrId::GroupArrivedLate))
                        .size(size::META)
                        .color(colour(t.text_secondary)),
                );
            }
            if self.menu == Some(node.id) {
                list = list.push(self.actions(node, t));
            }
        }
        list
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
                Some(false) => Some(open.bar.title.resolve(self.lang)),
                None => None,
            };
            let mut said = column![];
            if let Some(who) = who {
                said = said.push(text(who).size(13).color(meta));
            }
            said = said.push(text(quote.text.clone()).size(14).color(meta));
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
        if let Some(photo) = &node.photo {
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
            let line = text(format!("♪ voice message · {}", voice.duration))
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
        // An invitation is a card (1307): its words, the trust sentence and
        // Join, in place of the text a stock client reads.
        if let Some(invite) = &node.invite {
            inside = inside.push(self.invite_card(invite, !ours, t));
        } else if let Some(words) = &node.text {
            inside = inside.push(text(words.clone()).size(size::BODY).color(ink));
        }
        if let Some(fix) = &bubble.fix {
            let mut words = loquax::telemetry::coordinates(&fix.location);
            if let Some(accuracy) = loquax::telemetry::accuracy(&fix.location) {
                words = format!("{words}  {accuracy}");
            }
            // The newest report says whether the position is alive
            // (1209), in the scene's words; an older run its count.
            match &node.live {
                Some(live) => {
                    words.push_str(" · ");
                    words.push_str(&live.resolve(self.lang));
                }
                None => words.push_str(&loquax::telemetry::run_words(
                    bubble.updates,
                    bubble.since.as_ref(),
                )),
            }
            let stale = self.seen.now.is_some_and(|now| {
                fix.fix_time()
                    .saturating_add(loquax::telemetry::STALE_AFTER_SECS)
                    < now.unix_secs
            });
            let line_ink = match stale {
                true => meta,
                false => ink,
            };
            inside = inside.push(flat(
                text(words).size(size::BODY - 2.0).color(line_ink),
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
        actions = actions.push(link(
            self.word(StrId::Close),
            Message::Intent(Intent::Menu(None)),
            t,
        ));
        let panel = container(column![reactions, actions].spacing(4))
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
        // window can do; today it has none of them, so none show.
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
        // once there is (the scene's rule); the window's plus has no row
        // yet, so the round button is Send.
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
                open.bar.title.resolve(self.lang)
            ))
            .size(size::COMPOSE_TEXT)
            .height(Length::Shrink)
            .padding([6, 4])
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
fn list_row<'a>(r: &'a Row, now: Option<WallTime>, lang: Lang, t: Colours) -> Element<'a, Message> {
    let open = r.selected;
    let content = &r.row;
    let unread = content.unread > 0;
    let when = match (content.at, now) {
        (Some(at), Some(now)) => list_time(&at, &now),
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
    // sets it; a text of ours is said as ours.
    // A message typed and not sent stands in for it, in italics (1219).
    let invited = r
        .group
        .as_ref()
        .filter(|g| content.last.is_empty() || g.standing == loquax::ui::groups::Standing::Invited);
    let last = match (&r.draft, &r.live, content.ours && content.kind == "text") {
        (Some(draft), _, _) => draft.resolve(lang),
        // A group's row says it is an invitation, or how many are in it
        // while nothing was written yet (1307).
        (None, None, _) if invited.is_some() => {
            invited.map(|g| g.words.resolve(lang)).unwrap_or_default()
        }
        (None, Some(live), _) => format!("{} {}", chat::PIN, live.resolve(lang)),
        (None, None, true) => format!("{}: {}", StrId::QuoteYou.text(lang), content.last),
        (None, None, false) => content.last.clone(),
    };
    let style = match r.draft {
        Some(_) => iced::font::Style::Italic,
        None => iced::font::Style::Normal,
    };
    let words = column![
        text(content.name.clone())
            .size(size::ROW_NAME)
            .color(colour(t.text))
            .wrapping(text::Wrapping::None),
        text(last)
            .size(size::ROW_LAST)
            .color(colour(t.text_secondary))
            .font(iced::Font {
                style,
                ..iced::Font::DEFAULT
            })
            .wrapping(text::Wrapping::None),
    ]
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
