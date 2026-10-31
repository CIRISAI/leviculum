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
//! Everything said here comes from [`loquax::ui::chat`], what the phone
//! draws from too; the colours and sizes are its tokens. This file only
//! decides how iced draws them.

use iced::widget::canvas::{self, Canvas, Frame, Geometry, Path, Stroke};
use iced::widget::{
    button, column, container, mouse_area, row, scrollable, text, text_editor, text_input, Space,
};
use iced::{
    border, mouse, Alignment, Background, Border, Color, Element, Length, Point, Rectangle,
    Renderer, Theme,
};

use loquax::ui::chat::{self, size, Bubble, Colours, ListRow, Mark, Open, Placement};
use loquax::ui::reactions::SIGNAL_QUICK;
use loquax::when::{date_label, list_time, WallTime};

use super::{App, Message};

/// The list's width, in logical pixels.
pub const LIST_WIDTH: f32 = 340.0;

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

/// A peer's avatar: a circle of `diameter` with its initials.
fn avatar<'a>(
    peer: &[u8; 16],
    initials: &str,
    icon: Option<&(String, String)>,
    diameter: f32,
) -> Element<'a, Message> {
    let (fg, bg) = chat::avatar_colours(peer, icon);
    let (fg, bg) = (rgb(fg), rgb(bg));
    container(text(initials.to_string()).size(diameter * 0.4).color(fg))
        .width(diameter)
        .height(diameter)
        .center_x(diameter)
        .center_y(diameter)
        .style(move |_: &Theme| container::Style {
            background: Some(Background::Color(bg)),
            border: border::rounded(diameter / 2.0),
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
        let tick = |frame: &mut Frame, dx: f32| {
            let path = Path::new(|b| {
                b.move_to(p(0.12 + dx, 0.55));
                b.line_to(p(0.32 + dx, 0.75));
                b.line_to(p(0.72 + dx, 0.3));
            });
            frame.stroke(&path, line);
        };
        match self.icon {
            Icon::Mark(Mark::Queued) => {
                frame.stroke(&Path::circle(p(0.5, 0.5), s * 0.38), line);
                let hands = Path::new(|b| {
                    b.move_to(p(0.5, 0.25));
                    b.line_to(p(0.5, 0.5));
                    b.line_to(p(0.68, 0.6));
                });
                frame.stroke(&hands, line);
            }
            Icon::Mark(Mark::Sending) => {
                frame.stroke(&Path::circle(p(0.5, 0.5), s * 0.38), line);
            }
            Icon::Mark(Mark::Sent) => tick(&mut frame, 0.08),
            Icon::Mark(Mark::Delivered) => {
                tick(&mut frame, -0.06);
                tick(&mut frame, 0.18);
            }
            Icon::Mark(Mark::Failed) => {
                frame.fill(&Path::circle(p(0.5, 0.5), s * 0.45), self.failed.0);
                let sign = Stroke::default()
                    .with_color(self.failed.1)
                    .with_width(s / 8.0);
                frame.stroke(&Path::line(p(0.5, 0.25), p(0.5, 0.58)), sign);
                frame.fill(&Path::circle(p(0.5, 0.74), s * 0.07), self.failed.1);
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

    /// The list pane: its bar, the search, the new conversation's field
    /// when asked for, and the rows.
    pub(super) fn list_pane(&self) -> Element<'_, Message> {
        let t = self.tokens();
        let bar = row![
            text("Loquax").size(22).color(colour(t.text)),
            Space::new().width(Length::Fill),
            link("Map", Message::ShowMap(None), t),
            link(
                match self.dark() {
                    true => "Light",
                    false => "Dark",
                },
                Message::ToggleTheme,
                t
            ),
            link("New", Message::NewConversation, t),
        ]
        .align_y(Alignment::Center)
        .padding([12, 12]);
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
        let search = field(&self.search, "Search").on_input(Message::Search);
        let mut pane = column![bar, container(search).padding([0, 12])].spacing(8);
        if let Some(address) = &self.new_address {
            let new = field(address, "Address, 32 hex digits")
                .on_input(Message::NewAddress)
                .on_submit(Message::NewGo);
            pane = pane.push(container(new).padding([0, 12]));
        }
        let open = self.seen.chat.as_ref().map(|c| c.peer);
        let rows = self
            .seen
            .chats
            .iter()
            .filter(|r| chat::row_matches(r, &self.search))
            .fold(column![], |rows, r| {
                rows.push(list_row(r, open == Some(r.peer), self.seen.now, t))
            });
        let body: Element<'_, Message> = match self.seen.chats.is_empty() {
            true => container(
                text("No conversations yet")
                    .size(15)
                    .color(colour(t.text_secondary)),
            )
            .padding(16)
            .into(),
            false => scrollable(rows).height(Length::Fill).into(),
        };
        container(pane.push(body))
            .width(LIST_WIDTH)
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
        let Some(open) = &self.seen.chat else {
            return container(
                text("Pick a conversation")
                    .size(17)
                    .color(colour(t.text_secondary)),
            )
            .center(Length::Fill)
            .style(ground)
            .into();
        };
        let bar = container(
            row![
                avatar(
                    &open.peer,
                    &open.initials,
                    open.icon.as_ref(),
                    size::BAR_AVATAR
                ),
                text(open.name.clone())
                    .size(size::BAR_NAME)
                    .color(colour(t.text))
                    .width(Length::Fill),
                link("Map", Message::ShowMap(Some(open.peer)), t),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        )
        .height(size::BAR)
        .padding([0, 12])
        .align_y(Alignment::Center)
        .width(Length::Fill)
        .style(move |_: &Theme| container::Style {
            background: Some(Background::Color(colour(t.bar))),
            ..container::Style::default()
        });
        let messages = self.bubbles(open, t);
        column![bar, messages, self.compose_bar(open, t)]
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn bubbles<'a>(&'a self, open: &'a Open, t: Colours) -> Element<'a, Message> {
        let width = self.chat_width();
        let mut list = column![].padding([0, 12]);
        for (bubble, place) in open.bubbles.iter().zip(&open.placements) {
            if place.day {
                if let (Some(at), Some(now)) = (bubble.at, self.seen.now) {
                    list = list.push(
                        container(
                            text(date_label(&at, &now))
                                .size(size::DAY)
                                .color(colour(t.text_secondary)),
                        )
                        .center_x(Length::Fill)
                        .padding(iced::Padding::ZERO.top(16).bottom(8)),
                    );
                }
            }
            let gap = match place.joins_before {
                true => size::GAP_GROUPED,
                false if place.day => 0.0,
                false => size::GAP,
            };
            list = list.push(Space::new().height(gap));
            list = list.push(self.bubble(open, bubble, place, width, t));
            if self.menu == Some(bubble.key) {
                list = list.push(self.actions(open, bubble, t));
            }
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

    /// The chat's width in logical pixels, for the bubbles' widest.
    fn chat_width(&self) -> f32 {
        (self.window.width - LIST_WIDTH).max(200.0)
    }

    fn bubble<'a>(
        &'a self,
        open: &'a Open,
        bubble: &'a Bubble,
        place: &Placement,
        width: f32,
        t: Colours,
    ) -> Element<'a, Message> {
        let ours = bubble.ours;
        let (ground, ink, meta) = match ours {
            true => (t.bubble_ours, t.bubble_ours_text, t.bubble_ours_meta),
            false => (t.bubble_theirs, t.bubble_theirs_text, t.bubble_theirs_meta),
        };
        let (ground, ink, meta) = (colour(ground), colour(ink), colour(meta));
        let mut inside = column![].spacing(4);
        if let Some(quote) = &bubble.quote {
            let who = match quote.ours {
                Some(true) => Some("You".to_string()),
                Some(false) => Some(open.name.clone()),
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
        if let Some(fix) = &bubble.fix {
            let mut words = loquax::telemetry::coordinates(&fix.location);
            if let Some(accuracy) = loquax::telemetry::accuracy(&fix.location) {
                words = format!("{words}  {accuracy}");
            }
            words.push_str(&loquax::telemetry::run_words(
                bubble.updates,
                bubble.since.as_ref(),
            ));
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
                Message::ShowMap(Some(open.peer)),
            ));
        }
        if place.meta && (bubble.at.is_some() || ours) {
            let mut line = row![].spacing(4).align_y(Alignment::Center);
            if let Some(at) = bubble.at {
                line = line.push(text(at.hh_mm()).size(size::META).color(meta));
            }
            if let Some(mark) = bubble.mark {
                let failed = (colour(t.failed_ground), colour(t.failed_mark));
                line = line.push(drawing(Icon::Mark(mark), meta, failed, 16.0));
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
        let pressed = mouse_area(shape).on_right_press(Message::Menu(Some(bubble.key)));
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

    /// The reaction row and the actions under a bubble a right click
    /// opened.
    fn actions<'a>(&'a self, open: &Open, bubble: &'a Bubble, t: Colours) -> Element<'a, Message> {
        let peer = open.peer;
        let mut reactions = row![].spacing(6);
        if let Some(id) = bubble.id {
            for quick in SIGNAL_QUICK {
                reactions = reactions.push(
                    flat(text(quick).size(20), Message::React(id, quick.to_string())).padding(4),
                );
            }
        }
        let mut actions = row![].spacing(4);
        if let Some(id) = bubble.id {
            actions = actions.push(link("Reply", Message::Reply(id), t));
        }
        actions = actions.push(link("Copy", Message::Copy(bubble.body.clone()), t));
        actions = actions.push(match self.deleting == Some(bubble.key) {
            true => flat(
                text("Delete, sure?")
                    .size(15)
                    .color(colour(t.failed_mark_on_panel)),
                Message::Delete(peer, bubble.key),
            )
            .padding([4, 8]),
            false => flat(
                text("Delete")
                    .size(15)
                    .color(colour(t.failed_mark_on_panel)),
                Message::AskDelete(bubble.key),
            )
            .padding([4, 8]),
        });
        actions = actions.push(link("Close", Message::Menu(None), t));
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
        match bubble.ours {
            true => side.align_right(Length::Fill).into(),
            false => side.align_left(Length::Fill).into(),
        }
    }

    /// The compose bar: the reply being written, the emoji row when open,
    /// the pill with its emoji button and field, the round send button.
    fn compose_bar<'a>(&'a self, open: &Open, t: Colours) -> Element<'a, Message> {
        let mut bar = column![].spacing(6).padding([8, 12]);
        if let Some((_, quoted)) = &self.reply {
            bar = bar.push(
                row![
                    text(format!("Reply to: {quoted}"))
                        .size(14)
                        .color(colour(t.text_secondary))
                        .width(Length::Fill),
                    link("Cancel", Message::CancelReply, t),
                ]
                .align_y(Alignment::Center),
            );
        }
        if self.emoji {
            let picks = SIGNAL_QUICK.iter().fold(row![].spacing(6), |picks, e| {
                picks.push(flat(text(*e).size(22), Message::Insert(e.to_string())).padding(4))
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
            Message::ToggleEmoji,
        )
        .padding(6);
        let editor = text_editor(&self.compose)
            .placeholder(format!("Message {}", open.name))
            .size(size::COMPOSE_TEXT)
            .height(Length::Shrink)
            .padding([6, 4])
            .on_action(Message::Compose)
            .key_binding(|press| {
                let enter = matches!(
                    press.key,
                    iced::keyboard::Key::Named(iced::keyboard::key::Named::Enter)
                );
                match (enter, press.modifiers.shift()) {
                    (true, false) => Some(text_editor::Binding::Custom(Message::Send)),
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
        .on_press(Message::Send)
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

/// One row of the list.
fn list_row<'a>(
    r: &'a ListRow,
    open: bool,
    now: Option<WallTime>,
    t: Colours,
) -> Element<'a, Message> {
    let unread = r.unread > 0;
    let when = match (r.at, now) {
        (Some(at), Some(now)) => list_time(&at, &now),
        _ => String::new(),
    };
    let time_ink = match unread {
        true => colour(t.accent),
        false => colour(t.text_secondary),
    };
    let mut side = column![text(when).size(size::ROW_TIME).color(time_ink)]
        .align_x(Alignment::End)
        .spacing(8);
    if unread {
        side = side.push(
            container(
                text(r.unread.to_string())
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
    } else if let Some(mark) = r.mark {
        let failed = (colour(t.list_failed_ground), colour(t.list_failed_mark));
        side = side.push(drawing(
            Icon::Mark(mark),
            colour(t.text_secondary),
            failed,
            18.0,
        ));
    }
    let last = match r.ours && r.kind == "text" {
        true => format!("You: {}", r.last),
        false => r.last.clone(),
    };
    let words = column![
        text(r.name.clone())
            .size(size::ROW_NAME)
            .color(colour(t.text))
            .wrapping(text::Wrapping::None),
        text(last)
            .size(size::ROW_LAST)
            .color(colour(t.text_secondary))
            .wrapping(text::Wrapping::None),
    ]
    .spacing(2)
    .width(Length::Fill)
    .clip(true);
    let line = row![
        avatar(&r.peer, &r.initials, r.icon.as_ref(), size::ROW_AVATAR),
        words,
        side
    ]
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
    .on_press(Message::Open(r.peer))
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
