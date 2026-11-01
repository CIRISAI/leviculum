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

use iced::widget::canvas::{self, Canvas, Frame, Geometry, Path, Stroke};
use iced::widget::{
    button, column, container, mouse_area, row, scrollable, stack, text, text_editor, text_input,
    toggler, tooltip, Space,
};
use iced::{
    border, mouse, Alignment, Background, Border, Color, Element, Length, Point, Rectangle,
    Renderer, Theme,
};

use loquax::ui::chat::{self, size, Colours, Mark};
use loquax::ui::intent::{Intent, Switch};
use loquax::ui::reactions::SIGNAL_QUICK;
use loquax::ui::scene::{
    self, Action, BubbleNode, Chat, Group, LayoutClass, Lead, Row, Tab, TabId,
};
use loquax::ui::strings::{Lang, StrId};
use loquax::when::{list_time, WallTime};

use super::{App, Message};

/// The list's width, in logical pixels.
pub const LIST_WIDTH: f32 = 340.0;

/// The tab rail's width in the Wide class, in logical pixels.
pub const RAIL: f32 = 72.0;

/// The search field's id: the top bar's search focuses it.
pub const SEARCH_FIELD: &str = "search";

/// The liveness ring's width around an avatar (1209): thin.
const RING: f32 = 2.0;

/// A token, `0xaarrggbb`, as iced's colour.
pub fn colour(argb: u32) -> Color {
    let [a, r, g, b] = argb.to_be_bytes();
    Color::from_rgba8(r, g, b, f32::from(a) / 255.0)
}

/// An `0xrrggbb` as an opaque colour.
fn rgb(value: u32) -> Color {
    colour(0xff00_0000 | value)
}

/// The tokens of the theme the window is in.
pub fn tokens(dark: bool) -> Colours {
    match dark {
        true => chat::DARK,
        false => chat::LIGHT,
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
    let dot = diameter * 0.3;
    let ink = colour(match up {
        true => t.status_up,
        false => t.status_down,
    });
    let dot =
        container(Space::new().width(dot).height(dot)).style(move |_: &Theme| container::Style {
            background: Some(Background::Color(ink)),
            border: border::rounded(dot / 2.0).width(2.0).color(colour(t.bar)),
            ..container::Style::default()
        });
    stack![
        avatar(a, diameter, t),
        container(dot)
            .width(diameter)
            .height(diameter)
            .align_right(diameter)
            .align_bottom(diameter),
    ]
    .into()
}

/// A scene's avatar: a circle of `diameter` with its initials in its
/// colours, ringed by the liveness of the peer's position (1209).
fn avatar<'a>(a: &scene::Avatar, diameter: f32, t: Colours) -> Element<'a, Message> {
    let (fg, bg) = (rgb(a.fg), rgb(a.bg));
    let ring = a.ring.map(|ring| colour(ring.colour(&t)));
    container(text(a.initials.clone()).size(diameter * 0.4).color(fg))
        .width(diameter)
        .height(diameter)
        .center_x(diameter)
        .center_y(diameter)
        .style(move |_: &Theme| container::Style {
            background: Some(Background::Color(bg)),
            border: match ring {
                Some(ink) => border::rounded(diameter / 2.0).width(RING).color(ink),
                None => border::rounded(diameter / 2.0),
            },
            ..container::Style::default()
        })
        .into()
}

/// The small drawings: the delivery marks, the send arrow, the emoji
/// button's face. Drawn, not set, so they need no font that has them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Icon {
    Mark(Mark),
    Send,
    Face,
    /// A bar's back: a chevron to the left.
    Back,
    /// The search: a lens.
    Search,
    /// A new conversation: a pencil.
    Pencil,
    /// The theme's toggle: a disc half filled.
    Theme,
    /// The chats' tab: a speech bubble.
    Chats,
    /// The map's tab: a pin.
    Map,
    /// The settings: a gear.
    Settings,
}

struct Drawing {
    icon: Icon,
    ink: Color,
    /// The failed mark's circle and sign.
    failed: (Color, Color),
}

impl canvas::Program<Message> for Drawing {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let s = bounds.width.min(bounds.height);
        let p = |x: f32, y: f32| Point::new(x * s, y * s);
        let line = Stroke::default().with_color(self.ink).with_width(s / 10.0);
        match self.icon {
            // The ladder (1232) from the shape the phone draws too.
            Icon::Mark(mark) => {
                let shape = mark.shape();
                let u = s / chat::GRID;
                let at = |(x, y): (f32, f32)| Point::new(x * u, y * u);
                let sign = match mark {
                    Mark::Failed => self.failed.1,
                    _ => self.ink,
                };
                if let Some(disc) = shape.disc {
                    let path = Path::circle(at((disc.x, disc.y)), disc.r * u);
                    frame.fill(&path, self.failed.0);
                }
                let pen = Stroke::default()
                    .with_color(sign)
                    .with_width(shape.width * u)
                    .with_line_cap(canvas::LineCap::Round)
                    .with_line_join(canvas::LineJoin::Round);
                if let Some(ring) = shape.ring {
                    frame.stroke(&Path::circle(at((ring.x, ring.y)), ring.r * u), pen);
                }
                for line in shape.lines {
                    let path = Path::new(|b| {
                        if let Some((&first, rest)) = line.split_first() {
                            b.move_to(at(first));
                            for &point in rest {
                                b.line_to(at(point));
                            }
                        }
                    });
                    frame.stroke(&path, pen);
                }
                if let Some(dot) = shape.dot {
                    frame.fill(&Path::circle(at((dot.x, dot.y)), dot.r * u), sign);
                }
            }
            Icon::Send => {
                let arrow = Path::new(|b| {
                    b.move_to(p(0.3, 0.25));
                    b.line_to(p(0.78, 0.5));
                    b.line_to(p(0.3, 0.75));
                    b.line_to(p(0.38, 0.5));
                    b.close();
                });
                frame.fill(&arrow, self.ink);
            }
            Icon::Back => {
                let chevron = Path::new(|b| {
                    b.move_to(p(0.62, 0.2));
                    b.line_to(p(0.32, 0.5));
                    b.line_to(p(0.62, 0.8));
                });
                frame.stroke(&chevron, line);
            }
            Icon::Search => {
                frame.stroke(&Path::circle(p(0.43, 0.43), s * 0.25), line);
                frame.stroke(&Path::line(p(0.61, 0.61), p(0.85, 0.85)), line);
            }
            Icon::Pencil => {
                let body = Path::new(|b| {
                    b.move_to(p(0.2, 0.8));
                    b.line_to(p(0.25, 0.6));
                    b.line_to(p(0.68, 0.17));
                    b.line_to(p(0.83, 0.32));
                    b.line_to(p(0.4, 0.75));
                    b.close();
                });
                frame.stroke(&body, line);
            }
            Icon::Theme => {
                frame.stroke(&Path::circle(p(0.5, 0.5), s * 0.33), line);
                let half = Path::new(|b| {
                    b.arc(canvas::path::Arc {
                        center: p(0.5, 0.5),
                        radius: s * 0.33,
                        start_angle: iced::Radians(-std::f32::consts::FRAC_PI_2),
                        end_angle: iced::Radians(std::f32::consts::FRAC_PI_2),
                    });
                    b.close();
                });
                frame.fill(&half, self.ink);
            }
            Icon::Chats => {
                let bubble = Path::new(|b| {
                    b.move_to(p(0.2, 0.25));
                    b.line_to(p(0.8, 0.25));
                    b.line_to(p(0.8, 0.65));
                    b.line_to(p(0.45, 0.65));
                    b.line_to(p(0.28, 0.8));
                    b.line_to(p(0.3, 0.65));
                    b.line_to(p(0.2, 0.65));
                    b.close();
                });
                frame.stroke(&bubble, line);
            }
            Icon::Map => {
                let pin = Path::new(|b| {
                    b.move_to(p(0.3, 0.5));
                    b.line_to(p(0.5, 0.86));
                    b.line_to(p(0.7, 0.5));
                });
                frame.stroke(&pin, line);
                frame.stroke(&Path::circle(p(0.5, 0.38), s * 0.22), line);
                frame.fill(&Path::circle(p(0.5, 0.38), s * 0.07), self.ink);
            }
            Icon::Settings => {
                frame.stroke(&Path::circle(p(0.5, 0.5), s * 0.2), line);
                for i in 0..6 {
                    let a = i as f32 * std::f32::consts::FRAC_PI_3;
                    let (sin, cos) = a.sin_cos();
                    frame.stroke(
                        &Path::line(
                            p(0.5 + 0.2 * cos, 0.5 + 0.2 * sin),
                            p(0.5 + 0.36 * cos, 0.5 + 0.36 * sin),
                        ),
                        line,
                    );
                }
            }
            Icon::Face => {
                frame.stroke(&Path::circle(p(0.5, 0.5), s * 0.38), line);
                frame.fill(&Path::circle(p(0.38, 0.42), s * 0.05), self.ink);
                frame.fill(&Path::circle(p(0.62, 0.42), s * 0.05), self.ink);
                let smile = Path::new(|b| {
                    b.arc(canvas::path::Arc {
                        center: p(0.5, 0.52),
                        radius: s * 0.2,
                        start_angle: iced::Radians(0.4),
                        end_angle: iced::Radians(std::f32::consts::PI - 0.4),
                    });
                });
                frame.stroke(&smile, line);
            }
        }
        vec![frame.into_geometry()]
    }
}

fn drawing<'a>(icon: Icon, ink: Color, failed: (Color, Color), side: f32) -> Element<'a, Message> {
    Canvas::new(Drawing { icon, ink, failed })
        .width(side)
        .height(side)
        .into()
}

/// A bare button over `content`: the colours are the content's.
fn flat<'a>(content: impl Into<Element<'a, Message>>, on: Message) -> button::Button<'a, Message> {
    button(content)
        .on_press(on)
        .padding(0)
        .style(|_: &Theme, _| button::Style::default())
}

/// A word on the bars, as a link in the accent.
fn link<'a>(words: &str, on: Message, t: Colours) -> Element<'a, Message> {
    flat(text(words.to_string()).size(15).color(colour(t.accent)), on)
        .padding([4, 8])
        .into()
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
        let face = drawing(
            icon,
            colour(t.accent),
            (Color::TRANSPARENT, Color::TRANSPARENT),
            24.0,
        );
        tooltip(
            flat(face, on).padding(8),
            container(text(label.to_string()).size(13).color(colour(t.text)))
                .padding([2, 6])
                .style(move |_: &Theme| container::Style {
                    background: Some(Background::Color(colour(t.panel))),
                    border: border::rounded(4),
                    ..container::Style::default()
                }),
            tooltip::Position::Bottom,
        )
        .into()
    }

    /// The scene's top bar over the list: the reader's avatar with the
    /// link's dot, the title, the bar's actions; then the window's own
    /// theme toggle.
    fn top_bar<'a>(&'a self, t: Colours) -> Element<'a, Message> {
        let top = &self.scene.top;
        let mut line = row![].spacing(12).align_y(Alignment::Center);
        if let Lead::Me { avatar, link } = &top.lead {
            line = line.push(avatar_with_dot(avatar, size::BAR_AVATAR, *link, t));
        }
        line = line.push(
            text(top.title.resolve(self.lang))
                .size(22)
                .color(colour(t.text))
                .width(Length::Fill),
        );
        let mut actions = row![].align_y(Alignment::Center);
        for action in top.actions.iter().filter(|a| a.enabled) {
            let icon = match action.intent {
                Intent::Search => Icon::Search,
                Intent::NewConversation => Icon::Pencil,
                _ => continue,
            };
            actions = actions.push(self.icon_action(
                icon,
                self.word(action.label_key),
                Message::Intent(action.intent.clone()),
                t,
            ));
        }
        actions = actions.push(self.icon_action(
            Icon::Theme,
            self.word(Intent::Theme.label()),
            Message::Intent(Intent::Theme),
            t,
        ));
        actions = actions.push(self.icon_action(
            Icon::Settings,
            self.word(StrId::Settings),
            Message::Settings,
            t,
        ));
        container(line.push(actions))
            .height(size::BAR)
            .padding([0, 12])
            .align_y(Alignment::Center)
            .width(Length::Fill)
            .into()
    }

    /// One tab: its icon over its label, the unread count beside the
    /// icon, the active one in the accent on the selected row's ground.
    fn tab<'a>(&self, tab: &'a Tab, active: bool, t: Colours) -> Element<'a, Message> {
        let ink = match active {
            true => colour(t.accent),
            false => colour(t.text_secondary),
        };
        let icon = match tab.id {
            TabId::Chats => Icon::Chats,
            TabId::Map => Icon::Map,
        };
        let mut top = row![drawing(
            icon,
            ink,
            (Color::TRANSPARENT, Color::TRANSPARENT),
            26.0
        )]
        .spacing(2)
        .align_y(Alignment::Center);
        if let Some(count) = tab.badge {
            top = top.push(
                container(
                    text(count.to_string())
                        .size(size::BADGE_TEXT - 2.0)
                        .color(colour(t.badge_text)),
                )
                .padding([0, 5])
                .style(move |_: &Theme| container::Style {
                    background: Some(Background::Color(colour(t.badge))),
                    border: border::rounded(size::BADGE_CORNER),
                    ..container::Style::default()
                }),
            );
        }
        let item = column![top, text(tab.label.resolve(self.lang)).size(12).color(ink)]
            .spacing(2)
            .align_x(Alignment::Center);
        let ground = match active {
            true => colour(t.selected_row),
            false => Color::TRANSPARENT,
        };
        button(container(item).center_x(Length::Fill).padding([8, 0]))
            .on_press(Message::Intent(Intent::Activate(scene::NodeId::Tab(
                tab.id,
            ))))
            .padding(0)
            .width(Length::Fill)
            .style(move |_: &Theme, _| button::Style {
                background: Some(Background::Color(ground)),
                border: border::rounded(12),
                ..button::Style::default()
            })
            .into()
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
        let ground = move |_: &Theme| container::Style {
            background: Some(Background::Color(colour(t.bar))),
            ..container::Style::default()
        };
        Some(match self.class() {
            LayoutClass::Wide => container(column(items).spacing(8).padding([12, 6]))
                .width(RAIL)
                .height(Length::Fill)
                .style(ground)
                .into(),
            LayoutClass::Compact => container(row(items).spacing(8).padding([6, 12]))
                .width(Length::Fill)
                .style(ground)
                .into(),
        })
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
                receipts
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

    /// The list pane: the scene's top bar, the search, the new
    /// conversation's field when asked for, and the rows.
    pub(super) fn list_pane(&self) -> Element<'_, Message> {
        let t = self.tokens();
        let bar = self.top_bar(t);
        let field = |value: &str, hint: &str| {
            text_input(hint, value)
                .size(15)
                .padding([8, 14])
                .style(move |_: &Theme, _| text_input::Style {
                    background: Background::Color(colour(t.bar)),
                    border: border::rounded(size::COMPOSE_CORNER),
                    icon: colour(t.text_secondary),
                    placeholder: colour(t.text_secondary),
                    value: colour(t.text),
                    selection: colour(t.selected_row),
                })
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
        column![bar, messages, self.compose_bar(open, t)]
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
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
        for node in &group.bubbles {
            let place = &node.place;
            let gap = match place.joins_before {
                true => size::GAP_GROUPED,
                false if place.day => 0.0,
                false => size::GAP,
            };
            list = list.push(Space::new().height(gap));
            list = list.push(self.bubble(open, node, width, t));
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
        if !bubble.body.is_empty() {
            inside = inside.push(text(bubble.body.clone()).size(size::BODY).color(ink));
        }
        // A photo (1220): its words, a press shows it whole in the
        // system's viewer. The thumbnail in the window is a later slice.
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
                let face = drawing(Icon::Mark(mark.mark), ink, failed, 16.0);
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
        let pill = container(row![face, editor].align_y(Alignment::Center))
            .width(Length::Fill)
            .padding([0, 6])
            .style(move |_: &Theme| container::Style {
                background: Some(Background::Color(colour(t.bar))),
                border: border::rounded(size::COMPOSE_CORNER),
                ..container::Style::default()
            });
        let send = button(
            container(drawing(
                Icon::Send,
                colour(t.on_send),
                (Color::TRANSPARENT, Color::TRANSPARENT),
                28.0,
            ))
            .center(size::SEND),
        )
        .on_press(send)
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
            container(
                text(content.unread.to_string())
                    .size(size::BADGE_TEXT)
                    .color(colour(t.badge_text)),
            )
            .height(size::BADGE)
            .padding([0, 7])
            .center_y(size::BADGE)
            .style(move |_: &Theme| container::Style {
                background: Some(Background::Color(colour(t.badge))),
                border: border::rounded(size::BADGE_CORNER),
                ..container::Style::default()
            }),
        );
    } else if let Some(mark) = content.mark {
        let failed = (colour(t.list_failed_ground), colour(t.list_failed_mark));
        under = under.push(drawing(
            Icon::Mark(mark),
            colour(chat::list_ink(mark, &t)),
            failed,
            18.0,
        ));
    }
    let side = side.push(under);
    // A report's line is the scene's words after the pin, as the phone
    // sets it; a text of ours is said as ours.
    // A message typed and not sent stands in for it, in italics (1219).
    let last = match (&r.draft, &r.live, content.ours && content.kind == "text") {
        (Some(draft), _, _) => draft.resolve(lang),
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
    let ground = match open {
        true => colour(t.selected_row),
        false => Color::TRANSPARENT,
    };
    button(
        container(line)
            .height(Length::Shrink)
            .padding([12, 16])
            .width(Length::Fill),
    )
    .on_press(Message::Intent(Intent::Activate(r.id)))
    .padding(0)
    .width(Length::Fill)
    .style(move |_: &Theme, status| button::Style {
        background: Some(Background::Color(match status {
            button::Status::Hovered if !open => colour(t.bar),
            _ => ground,
        })),
        ..button::Style::default()
    })
    .into()
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
