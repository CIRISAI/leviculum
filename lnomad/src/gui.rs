//! `lnomad --gui`: the browser in a window (slice 2 of
//! `docs/src/concepts/lnomad-everywhere.md`), drawn from the same scene
//! the terminal draws ([`Model::scene`], 1255) with the window widgets
//! Loquax's window draws with (`leviculum-ui-iced`).
//!
//! The page is the scene's window of laid-out lines, set in a monospace
//! face on a grid of cells, one rich text per line and one span per run:
//! the one layout every target draws, no second micron renderer. A click
//! on a link run is `Activate(Link(n))`, a press on a bar's action its
//! intent, a key the rows table's chord ([`crate::app::keys`]); the
//! address is the window's own text field, its Enter the field's go.
//!
//! Two halves, as in the terminal: the model and its view on iced's
//! thread, and the transfers ([`crate::transfer`]) on a tokio runtime
//! beside it, which the window hands the effects that leave the process
//! and which answers with outcomes the window folds into the model. The
//! window draws no inline pictures yet; a picture link stays a link.

use std::borrow::Cow;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use iced::widget::text::Span;
use iced::widget::{column, container, mouse_area, rich_text, row, scrollable, text, Space};
use iced::{
    border, keyboard, mouse, Alignment, Background, Color, Element, Font, Length, Size,
    Subscription, Task, Theme as IcedTheme,
};
use tokio::sync::mpsc as tokio_mpsc;

use leviculum_ui::color::Ink;
use leviculum_ui::input::KeyModifiers;
use leviculum_ui_iced::{chrome, control, Icon, Palette};

use crate::app::intent::apply;
use crate::app::model::{CHROME_ROWS, SCROLLBAR_COLS};
use crate::app::scene::{AddressField, Lead, NodeId, Page, Row, Run, Scene, Sheet};
use crate::app::strings::{with, StrId, Text};
use crate::app::{
    show_start_screen, split_path_anchor, update, AppEvent, Effect, HistoryAction, Intent, Mode,
    Model, MouseEvent, MouseEventKind, Pending, ToastKind,
};
use crate::browser::BrowserOptions;
use crate::discovery::DiscoveredNode;
use crate::fetch::Session;
use crate::theme::Theme;
use crate::transfer::{
    close_session, spawn_discovery, spawn_download, spawn_fetch, FetchOutcome, LoadResult,
};
use crate::url::Target;
use tui_input::Input;

/// The page's face: Fira Mono, embedded so a page and its golden agree on
/// every host (`lnomad/fonts/OFL.txt`).
pub const MONO_BYTES: &[u8] = include_bytes!("../fonts/FiraMono-Medium.ttf");

/// The page's face as iced names it: the family, at the one weight the
/// file holds.
pub const MONO: Font = Font {
    family: iced::font::Family::Name("Fira Mono"),
    weight: iced::font::Weight::Medium,
    ..Font::DEFAULT
};

/// The page's text size, in logical pixels.
pub const PAGE_TEXT: f32 = 14.0;

/// A cell's width: Fira Mono's advance, 600 of its 1000 units, at
/// [`PAGE_TEXT`].
pub const CELL_W: f32 = PAGE_TEXT * 0.6;

/// A cell's height: the line pitch of the page.
pub const CELL_H: f32 = 18.0;

/// The page's margin around the grid.
pub const PAGE_PAD: f32 = 12.0;

/// The top bar's height.
pub const BAR: f32 = 52.0;

/// The address row's height.
pub const ADDRESS_ROW: f32 = 48.0;

/// The status line's height.
pub const STATUS_ROW: f32 = 28.0;

/// A sheet's width beside the page.
pub const SHEET_WIDTH: f32 = 360.0;

/// The window's size at the start, the size the goldens are drawn at.
pub const WINDOW: (f32, f32) = (1280.0, 800.0);

/// The lines a wheel notch scrolls, as the terminal's wheel does.
const WHEEL_LINES: usize = 3;

/// The window's grounds and words in the theme the model is in. The
/// page's own colours are the document's; these are the chrome's.
pub fn palette(theme: Theme) -> Palette {
    let link = theme.link_fg();
    let accent = Color::from_rgb8(link.0, link.1, link.2);
    match theme {
        Theme::Light => Palette {
            background: Color::from_rgb8(0xff, 0xff, 0xff),
            bar: Color::from_rgb8(0xf0, 0xf2, 0xf5),
            panel: Color::from_rgb8(0xe4, 0xe7, 0xec),
            text: Color::from_rgb8(0x1b, 0x1d, 0x21),
            text_secondary: Color::from_rgb8(0x5f, 0x65, 0x6e),
            accent,
            selected_row: Color::from_rgb8(0xdc, 0xe8, 0xf7),
            badge: accent,
            badge_text: Color::WHITE,
        },
        Theme::Dark => Palette {
            background: Color::from_rgb8(0x14, 0x15, 0x18),
            bar: Color::from_rgb8(0x22, 0x24, 0x29),
            panel: Color::from_rgb8(0x2e, 0x31, 0x37),
            text: Color::from_rgb8(0xe6, 0xe8, 0xeb),
            text_secondary: Color::from_rgb8(0x9a, 0xa0, 0xa8),
            accent,
            selected_row: Color::from_rgb8(0x1f, 0x3a, 0x5c),
            badge: accent,
            badge_text: Color::BLACK,
        },
    }
}

/// An ink of the page as the window's colour: RGB as it is, a palette
/// index through the xterm table.
pub fn ink(ink: Ink) -> Color {
    let (r, g, b) = match ink {
        Ink::Rgb(r, g, b) => (r, g, b),
        Ink::Indexed(n) => indexed(n),
    };
    Color::from_rgb8(r, g, b)
}

/// The xterm-256 colour of palette index `n`.
fn indexed(n: u8) -> (u8, u8, u8) {
    const ANSI: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (205, 0, 0),
        (0, 205, 0),
        (205, 205, 0),
        (0, 0, 238),
        (205, 0, 205),
        (0, 205, 205),
        (229, 229, 229),
        (127, 127, 127),
        (255, 0, 0),
        (0, 255, 0),
        (255, 255, 0),
        (92, 92, 255),
        (255, 0, 255),
        (0, 255, 255),
        (255, 255, 255),
    ];
    match n {
        0..=15 => ANSI[usize::from(n)],
        16..=231 => {
            let i = n - 16;
            let level = |c: u8| match c {
                0 => 0,
                c => 55 + c * 40,
            };
            (level(i / 36), level((i / 6) % 6), level(i % 6))
        }
        232..=255 => {
            let v = 8 + (n - 232) * 10;
            (v, v, v)
        }
    }
}

/// The cells a page of `w` by `h` pixels holds, as the model's terminal
/// size: the page's columns plus the scrollbar's the layout keeps free,
/// its rows plus the chrome's rows the model takes off.
pub fn cells(w: f32, h: f32) -> (u16, u16) {
    let page_w = (w - 2.0 * PAGE_PAD).max(CELL_W);
    let page_h = (h - BAR - ADDRESS_ROW - STATUS_ROW - 2.0 * PAGE_PAD).max(CELL_H);
    let cols = (page_w / CELL_W).floor() as u16;
    let rows = (page_h / CELL_H).floor() as u16;
    (
        cols.saturating_add(SCROLLBAR_COLS),
        rows.saturating_add(CHROME_ROWS),
    )
}

#[derive(Clone, Debug)]
pub enum Message {
    /// A press on a node, a bar's action, a link.
    Intent(Intent),
    /// What the address field holds now.
    Address(String),
    /// Enter in the address field.
    AddressGo,
    /// A key nothing in the window took.
    Key(keyboard::Event),
    /// The wheel over the page, lines down (positive) or up.
    Wheel(f32),
    /// The window has this logical size.
    Resized(Size),
    /// A transfer's outcome.
    Outcome(Outcome),
    /// A frame is drawn: the spinner's and the toast's clock.
    Frame(Instant),
}

/// What the transfers answer.
#[derive(Clone, Debug)]
pub enum Outcome {
    Loaded {
        doc: leviculum_micron::MicronDocument,
        title: String,
        dest: [u8; 16],
        identifying: bool,
    },
    Failed(Text),
    Downloaded {
        name: String,
        bytes: usize,
        dir: String,
    },
    DownloadFailed(Text),
    Node(DiscoveredNode),
    Say(ToastKind, Text),
}

/// The browser's window: the model, and where effects that leave the
/// process go.
pub struct Window {
    pub model: Model,
    jobs: Option<tokio_mpsc::UnboundedSender<Effect>>,
    /// When the model last ticked.
    ticked: Option<Instant>,
}

impl Window {
    /// A window over `model`, `jobs` the transfers' inbox; none in a test,
    /// where an effect that would leave the process is dropped.
    pub fn new(mut model: Model, jobs: Option<tokio_mpsc::UnboundedSender<Effect>>) -> Self {
        // No inline pictures in the window yet: the model does not ask for
        // them, and a picture link stays a link.
        model.no_images = true;
        let (cols, rows) = cells(WINDOW.0, WINDOW.1);
        update(&mut model, AppEvent::Resize(cols, rows));
        Window {
            model,
            jobs,
            ticked: None,
        }
    }

    pub fn palette(&self) -> Palette {
        palette(self.model.theme)
    }

    pub fn theme(&self) -> IcedTheme {
        match self.model.theme {
            Theme::Light => IcedTheme::Light,
            Theme::Dark => IcedTheme::Dark,
        }
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        let effects = match message {
            Message::Intent(intent) => apply(&mut self.model, intent),
            Message::Address(value) => {
                let mut effects = Vec::new();
                if self.model.mode != Mode::Address {
                    effects = apply(&mut self.model, Intent::Activate(NodeId::Address));
                }
                self.model.input = Input::new(value);
                effects
            }
            Message::AddressGo => apply(&mut self.model, Intent::Go { url: None }),
            Message::Key(event) => match leviculum_ui_iced::key::chord(&event) {
                Some(key) => update(&mut self.model, AppEvent::Key(key)),
                None => Vec::new(),
            },
            Message::Wheel(lines) => {
                let kind = match lines > 0.0 {
                    true => MouseEventKind::ScrollDown,
                    false => MouseEventKind::ScrollUp,
                };
                let notches = (lines.abs() / WHEEL_LINES as f32).ceil().max(1.0) as usize;
                let mut effects = Vec::new();
                for _ in 0..notches {
                    effects.extend(update(
                        &mut self.model,
                        AppEvent::Mouse(MouseEvent {
                            kind,
                            column: 0,
                            row: 0,
                            modifiers: KeyModifiers::NONE,
                        }),
                    ));
                }
                effects
            }
            Message::Resized(size) => {
                let (cols, rows) = cells(size.width, size.height);
                update(&mut self.model, AppEvent::Resize(cols, rows))
            }
            Message::Outcome(outcome) => self.fold(outcome),
            Message::Frame(now) => match self.ticked {
                Some(last) if now.duration_since(last) < Duration::from_millis(SPINNER_TICK_MS) => {
                    Vec::new()
                }
                _ => {
                    self.ticked = Some(now);
                    update(&mut self.model, AppEvent::Tick)
                }
            },
        };
        self.run(effects)
    }

    /// A transfer's outcome folded into the model as the terminal folds it.
    fn fold(&mut self, outcome: Outcome) -> Vec<Effect> {
        match outcome {
            Outcome::Loaded {
                doc,
                title,
                dest,
                identifying,
            } => {
                let effects = update(&mut self.model, AppEvent::PageLoaded { doc, title });
                self.model.note_identify(dest, identifying);
                effects
            }
            Outcome::Failed(text) => update(&mut self.model, AppEvent::LoadFailedText(text)),
            Outcome::Downloaded { name, bytes, dir } => {
                update(&mut self.model, AppEvent::DownloadDone { name, bytes, dir })
            }
            Outcome::DownloadFailed(text) => {
                update(&mut self.model, AppEvent::DownloadFailed(text))
            }
            Outcome::Node(node) => update(&mut self.model, AppEvent::NodeDiscovered(node)),
            Outcome::Say(kind, text) => {
                self.model.say(kind, text);
                Vec::new()
            }
        }
    }

    /// The effects the window does itself, the rest handed to the
    /// transfers.
    fn run(&mut self, effects: Vec<Effect>) -> Task<Message> {
        let mut tasks = Vec::new();
        for effect in effects {
            match effect {
                Effect::Quit => {
                    self.model.quit = true;
                    tasks.push(iced::exit());
                }
                Effect::Copy(text) => tasks.push(iced::clipboard::write(text)),
                Effect::SaveBookmarks => {
                    if let Some(path) = &self.model.bookmarks_path {
                        let _ = self.model.bookmarks.save(path);
                    }
                }
                Effect::SavePins => {
                    if let Some(path) = &self.model.pins_path {
                        let _ = self.model.pins.save(path);
                    }
                }
                Effect::OpenExternal(url) => match open::that(&url) {
                    Ok(_) => self
                        .model
                        .say(ToastKind::Info, with(StrId::OpenedExternally, url)),
                    Err(_) => self
                        .model
                        .say(ToastKind::Error, with(StrId::FailedToOpen, url)),
                },
                // No pictures in the window yet (see the module's doc).
                Effect::FetchImages(_) | Effect::SaveImage { .. } | Effect::OpenImage { .. } => {}
                leaving => {
                    if let Some(jobs) = &self.jobs {
                        let _ = jobs.send(leaving);
                    }
                }
            }
        }
        Task::batch(tasks)
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let mut all = vec![
            iced::event::listen_with(uncaptured_key),
            iced::window::resize_events().map(|(_, size)| Message::Resized(size)),
        ];
        // iced's timer needs a tokio or smol executor this window does
        // not carry; the window's frames are its clock, thinned to the
        // terminal's tick.
        if self.model.needs_tick() {
            all.push(iced::window::frames().map(Message::Frame));
        }
        Subscription::batch(all)
    }

    pub fn view(&self) -> Element<'_, Message> {
        let scene = self.model.scene();
        let p = self.palette();
        let lang = self.model.lang;
        let bar = top_bar(&scene, lang, p);
        let address = address_row(&scene.address, &scene, lang, p);
        let page = page_view(&scene.page, p);
        let page: Element<'_, Message> = mouse_area(page)
            .on_scroll(|delta| {
                Message::Wheel(match delta {
                    mouse::ScrollDelta::Lines { y, .. } => -y * WHEEL_LINES as f32,
                    mouse::ScrollDelta::Pixels { y, .. } => -y / CELL_H,
                })
            })
            .into();
        let sheet = scene
            .names
            .as_ref()
            .or(scene.places.as_ref())
            .map(|sheet| sheet_view(sheet, lang, p));
        let middle: Element<'_, Message> = match sheet {
            Some(sheet) => row![page, sheet].height(Length::Fill).into(),
            None => page,
        };
        let status = status_row(&scene, lang, p);
        container(column![bar, address, middle, status])
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_: &IcedTheme| container::Style {
                background: Some(Background::Color(p.background)),
                ..container::Style::default()
            })
            .into()
    }
}

/// How often the spinner and the toast's clock tick, as in the terminal.
const SPINNER_TICK_MS: u64 = 100;

/// A key the window's widgets did not take: typing in the address field
/// stays the field's.
fn uncaptured_key(
    event: iced::Event,
    status: iced::event::Status,
    _: iced::window::Id,
) -> Option<Message> {
    match (event, status) {
        (iced::Event::Keyboard(key), iced::event::Status::Ignored) => Some(Message::Key(key)),
        _ => None,
    }
}

/// The scene's top bar: back while history has a past, the page's title,
/// its actions as words in the accent, a disabled one in the secondary.
fn top_bar<'a>(scene: &Scene, lang: crate::app::strings::Lang, p: Palette) -> Element<'a, Message> {
    let bar = &scene.top_bar;
    let lead = match &bar.lead {
        Lead::Back { back, .. } => Some(control::icon_action(
            Icon::Back,
            back.label_key.text(lang),
            Message::Intent(back.intent.clone()),
            p,
        )),
        Lead::Me { .. } => None,
    };
    let actions = bar.actions.iter().map(|action| {
        let words = action.label_key.text(lang);
        match action.enabled {
            true => control::link(words, Message::Intent(action.intent.clone()), p.accent),
            false => container(text(words.to_string()).size(15).color(p.text_secondary))
                .padding([4, 8])
                .into(),
        }
    });
    let title = match bar.title.resolve(lang) {
        title if title.is_empty() => StrId::Page.text(lang).to_string(),
        title => title,
    };
    let bar = chrome::top_bar(lead, title, actions.collect::<Vec<_>>(), BAR, p);
    container(bar)
        .style(move |_: &IcedTheme| container::Style {
            background: Some(Background::Color(p.bar)),
            ..container::Style::default()
        })
        .into()
}

/// The address field and the top bar's state chips beside it.
fn address_row<'a>(
    field: &AddressField,
    scene: &Scene,
    lang: crate::app::strings::Lang,
    p: Palette,
) -> Element<'a, Message> {
    let input = chrome::search_field(&field.hint.resolve(lang), &field.value, 18.0, p)
        .id(ADDRESS_FIELD)
        .on_input(Message::Address)
        .on_submit(Message::AddressGo)
        .width(Length::Fill);
    let chips = scene.state.iter().fold(
        row![].spacing(6).align_y(Alignment::Center),
        |chips, chip| chips.push(pill(chip.text.resolve(lang), p)),
    );
    container(row![input, chips].spacing(10).align_y(Alignment::Center))
        .height(ADDRESS_ROW)
        .padding([6, 12])
        .align_y(Alignment::Center)
        .into()
}

/// The address field's id.
pub const ADDRESS_FIELD: &str = "address";

/// A chip's words on the panel's ground.
fn pill<'a>(words: String, p: Palette) -> Element<'a, Message> {
    container(text(words).size(13).color(p.text_secondary))
        .padding([3, 10])
        .style(move |_: &IcedTheme| container::Style {
            background: Some(Background::Color(p.panel)),
            border: border::rounded(12),
            ..container::Style::default()
        })
        .into()
}

/// One run as a span: its colours, underline, the link it is part of; the
/// focused link on the selection's ground, a search match on the accent's.
fn span<'a>(run: &Run, focused: Option<usize>, p: Palette) -> Span<'a, usize, Font> {
    let style = run.style;
    let (mut fg, mut bg) = (style.fg.map(ink), style.bg.map(ink));
    if style.reverse {
        (fg, bg) = (Some(bg.unwrap_or(p.background)), Some(fg.unwrap_or(p.text)));
    }
    if run.link.is_some() && run.link == focused {
        bg = Some(p.selected_row);
    }
    match run.matched {
        Some(true) => bg = Some(p.accent),
        Some(false) => bg = Some(p.panel),
        None => {}
    }
    // One face for every run: the file holds one weight and no italic, and
    // a face asked for that it lacks falls back to the proportional sans,
    // which takes the run off the grid. Bold and italic are not drawn yet.
    let mut span = Span::new(run.text.clone())
        .font(MONO)
        .color(fg.unwrap_or(p.text))
        .underline(style.underline);
    if let Some(bg) = bg {
        span = span.background(Background::Color(bg));
    }
    span.link_maybe(run.link)
}

/// The page: its window of lines on the cell grid, one rich text per
/// line, a click on a link run its activation.
fn page_view<'a>(page: &Page, p: Palette) -> Element<'a, Message> {
    let focused = match page.focus {
        Some(crate::app::Focus::Link(n)) => Some(n),
        _ => None,
    };
    let lines = page.lines.iter().fold(column![], |lines, line| {
        let spans: Vec<Span<'a, usize, Font>> =
            line.runs.iter().map(|run| span(run, focused, p)).collect();
        let line: Element<'a, Message> = match spans.is_empty() {
            true => Space::new().height(CELL_H).into(),
            false => rich_text(spans)
                .size(PAGE_TEXT)
                .line_height(iced::Pixels(CELL_H))
                .font(MONO)
                .wrapping(text::Wrapping::None)
                .on_link_click(|n| Message::Intent(Intent::Activate(NodeId::Link(n))))
                .into(),
        };
        lines.push(container(line).height(CELL_H))
    });
    container(lines)
        .padding(PAGE_PAD)
        .width(Length::Fill)
        .height(Length::Fill)
        .clip(true)
        .into()
}

/// A sheet beside the page: the places panel or the name results, each
/// item the shared list item, a press its activation.
fn sheet_view<'a>(
    sheet: &Sheet,
    lang: crate::app::strings::Lang,
    p: Palette,
) -> Element<'a, Message> {
    let title = text(sheet.title.resolve(lang)).size(18).color(p.text);
    let rows = sheet.rows.iter().fold(column![], |rows, r| match r {
        Row::Heading(words) => rows.push(
            container(text(words.resolve(lang)).size(13).color(p.text_secondary)).padding([10, 16]),
        ),
        Row::Placeholder(words) => rows.push(
            container(text(words.resolve(lang)).size(14).color(p.text_secondary)).padding([6, 16]),
        ),
        Row::Item { item, title, .. } => rows.push(chrome::list_item(
            text(title.resolve(lang))
                .size(15)
                .color(p.text)
                .wrapping(text::Wrapping::None),
            item.selected,
            Message::Intent(item.activate.intent.clone()),
            p,
        )),
        Row::Note { text: words, .. } => rows.push(
            container(text(words.resolve(lang)).size(13).color(p.text_secondary)).padding([8, 16]),
        ),
    });
    container(column![
        container(title).padding([12, 16]),
        scrollable(rows)
    ])
    .width(SHEET_WIDTH)
    .height(Length::Fill)
    .style(move |_: &IcedTheme| container::Style {
        background: Some(Background::Color(p.bar)),
        ..container::Style::default()
    })
    .into()
}

/// The status line: what loads or where the focused link goes, and the
/// toast.
fn status_row<'a>(
    scene: &Scene,
    lang: crate::app::strings::Lang,
    p: Palette,
) -> Element<'a, Message> {
    let status = scene
        .status
        .as_ref()
        .map_or(String::new(), |chip| chip.text.resolve(lang));
    let mut line = row![text(status)
        .size(13)
        .color(p.text_secondary)
        .width(Length::Fill)
        .wrapping(text::Wrapping::None)]
    .align_y(Alignment::Center);
    if let Some(toast) = &scene.toast {
        let ink = match toast.kind {
            ToastKind::Error => Color::from_rgb8(0xd0, 0x30, 0x30),
            _ => p.text,
        };
        line = line.push(
            container(text(toast.text.resolve(lang)).size(13).color(ink))
                .padding([2, 10])
                .style(move |_: &IcedTheme| container::Style {
                    background: Some(Background::Color(p.panel)),
                    border: border::rounded(6),
                    ..container::Style::default()
                }),
        );
    }
    container(line)
        .height(STATUS_ROW)
        .padding([0, 12])
        .align_y(Alignment::Center)
        .style(move |_: &IcedTheme| container::Style {
            background: Some(Background::Color(p.bar)),
            ..container::Style::default()
        })
        .into()
}

/// Load the page's face into iced's font system, for a view drawn without
/// a window (the goldens); a window loads it through its settings.
pub fn load_mono() {
    let system = iced::advanced::graphics::text::font_system();
    let mut system = system.write().unwrap_or_else(PoisonError::into_inner);
    system.load_font(Cow::Borrowed(MONO_BYTES));
}

/// The transfers: the effects that leave the process, run over the
/// session, answered as outcomes. Ends when the window drops its inbox.
async fn transfers(
    session: Session,
    initial: Option<Target>,
    timeout: Duration,
    mut jobs: tokio_mpsc::UnboundedReceiver<Effect>,
    out: UnboundedSender<Outcome>,
) {
    let (node_tx, mut node_rx) = tokio_mpsc::unbounded_channel::<DiscoveredNode>();
    let mut session = session;
    session.set_announce_sink(node_tx);
    let session = Arc::new(tokio::sync::Mutex::new(session));
    let (tx, mut rx) = tokio_mpsc::unbounded_channel::<FetchOutcome>();
    let mut inflight: Option<tokio::task::JoinHandle<()>> = None;
    let mut generation: u64 = 0;
    let discovery = spawn_discovery(&session);
    if let Some(target) = initial {
        spawn_fetch(
            &mut inflight,
            &mut generation,
            &session,
            &tx,
            timeout,
            target,
        );
    }
    loop {
        tokio::select! {
            job = jobs.recv() => match job {
                Some(Effect::Navigate(target)) => {
                    spawn_fetch(&mut inflight, &mut generation, &session, &tx, timeout, target);
                }
                Some(Effect::Download(target)) => {
                    spawn_download(&mut inflight, &mut generation, &session, &tx, timeout, target);
                }
                Some(Effect::Cancel) => {
                    if let Some(handle) = inflight.take() {
                        handle.abort();
                    }
                    generation = generation.wrapping_add(1);
                }
                Some(Effect::SetIdentify { dest, on }) => {
                    if let Some(handle) = inflight.take() {
                        handle.abort();
                    }
                    generation = generation.wrapping_add(1);
                    if let Err(err) = session.lock().await.set_identify(&dest, on).await {
                        let _ = out.unbounded_send(Outcome::Say(
                            ToastKind::Error,
                            with(StrId::IdentifyNotSaved, err.to_string()),
                        ));
                    }
                }
                Some(_) => {}
                None => break,
            },
            Some(outcome) = rx.recv() => {
                if outcome.generation == generation {
                    inflight = None;
                    let outcome = match outcome.result {
                        LoadResult::Ok { doc, title, dest, identifying } => {
                            Outcome::Loaded { doc, title, dest, identifying }
                        }
                        LoadResult::Err(text) => Outcome::Failed(text),
                        LoadResult::Downloaded { name, bytes, dir } => {
                            Outcome::Downloaded { name, bytes, dir }
                        }
                        LoadResult::DownloadErr(text) => Outcome::DownloadFailed(text),
                    };
                    let _ = out.unbounded_send(outcome);
                }
            }
            Some(node) = node_rx.recv() => {
                let _ = out.unbounded_send(Outcome::Node(node));
            }
        }
    }
    if let Some(handle) = inflight.take() {
        handle.abort();
        let _ = handle.await;
    }
    discovery.abort();
    let _ = discovery.await;
    drop(tx);
    if let Some(message) = close_session(session).await {
        eprintln!("{message}");
    }
}

/// Open the window over `session` and run it until it is closed: the
/// model as the terminal sets it up, the transfers on the runtime the
/// session was made on, the window on this thread.
pub async fn run_gui(
    session: Session,
    initial: Option<Target>,
    opts: BrowserOptions,
    theme: Theme,
) -> iced::Result {
    let bookmarks_path = crate::bookmarks::default_path();
    let bookmarks = bookmarks_path
        .as_deref()
        .map(crate::bookmarks::Bookmarks::load)
        .unwrap_or_default();
    let pins_path = crate::names::default_path();
    let pins = pins_path
        .as_deref()
        .map(crate::names::PinStore::load)
        .unwrap_or_default();
    let mut model = Model {
        no_color: opts.no_color,
        depth: opts.depth,
        theme,
        bookmarks,
        bookmarks_path,
        pins,
        pins_path,
        fingerprint: session.fingerprint_hex(),
        lang: crate::app::strings::locale_lang(|name| std::env::var(name).ok()),
        ..Model::default()
    };
    let initial = match initial {
        Some(target) => {
            let (target, anchor) = split_path_anchor(target);
            model.pending = Some(Pending {
                target: target.clone(),
                action: HistoryAction::Push,
            });
            model.pending_anchor = anchor;
            Some(target)
        }
        None => {
            show_start_screen(&mut model);
            None
        }
    };
    let (jobs_tx, jobs_rx) = tokio_mpsc::unbounded_channel::<Effect>();
    let (out_tx, out_rx) = unbounded::<Outcome>();
    let driver = tokio::spawn(transfers(session, initial, opts.timeout, jobs_rx, out_tx));
    // iced asks its boot function for the first state once; what it needs
    // is handed over through this slot.
    let slot = Mutex::new(Some((model, jobs_tx, out_rx)));
    // The window's loop blocks this thread, which winit wants to be the
    // main one; the runtime's other workers keep the transfers going.
    let result = tokio::task::block_in_place(move || {
        iced::application(
            move || boot(slot.lock().unwrap_or_else(PoisonError::into_inner).take()),
            Window::update,
            Window::view,
        )
        .title("lnomad")
        .subscription(Window::subscription)
        .theme(Window::theme)
        .font(MONO_BYTES)
        .window_size(Size::new(WINDOW.0, WINDOW.1))
        .run()
    });
    // The window dropped its inbox: the transfers close the session.
    let _ = driver.await;
    result
}

type Handed = (
    Model,
    tokio_mpsc::UnboundedSender<Effect>,
    UnboundedReceiver<Outcome>,
);

fn boot(handed: Option<Handed>) -> (Window, Task<Message>) {
    match handed {
        Some((model, jobs, outcomes)) => (
            Window::new(model, Some(jobs)),
            Task::run(outcomes, Message::Outcome),
        ),
        None => (Window::new(Model::default(), None), Task::none()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_micron::parse;
    use std::path::PathBuf;
    use unicode_width::UnicodeWidthStr;

    const README: &str = include_str!("../tests_data/reticulum_readme.mu");

    /// The node the README is served from, the scene golden's.
    const NODE: &str = "c0ffee00112233445566778899aabbcc";

    /// The window over the README in `theme`, as a load leaves it: the
    /// page in history, its node known, nothing focused.
    fn readme(theme: Theme) -> Window {
        load_mono();
        let (cols, rows) = cells(WINDOW.0, WINDOW.1);
        let doc = parse(README);
        let mut model = Model::from_document(
            &doc,
            crate::app::model::content_width(cols),
            "Reticulum · README",
            (cols, rows),
        );
        let target = crate::url::parse_url(&format!("{NODE}:/page/readme.mu"), None).expect("url");
        model.current_dest = Some(target.dest_hash);
        model.history.visit(target);
        model.theme = theme;
        model.relayout(crate::app::model::content_width(cols));
        Window::new(model, None)
    }

    fn shoot(window: &Window) -> Vec<u8> {
        let p = window.palette();
        leviculum_ui_iced::headless::render(
            window.view(),
            &window.theme(),
            p.text,
            p.background,
            WINDOW.0 as u32,
            WINDOW.1 as u32,
        )
        .expect("the software renderer")
    }

    fn encode(rgb: &[u8], w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        let mut e = png::Encoder::new(&mut out, w, h);
        e.set_color(png::ColorType::Rgb);
        e.set_depth(png::BitDepth::Eight);
        let mut writer = e.write_header().expect("header");
        writer.write_image_data(rgb).expect("data");
        writer.finish().expect("finish");
        out
    }

    /// Hold `rgb` to the PNG golden `name` under `lnomad/tests/golden`,
    /// pixel for pixel. A picture that moved is left in the target dir;
    /// `LNOMAD_WRITE_GOLDEN=1` writes the golden instead.
    fn same_as(name: &str, rgb: &[u8], w: u32, h: u32) {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let path = root.join("tests/golden").join(name);
        if std::env::var_os("LNOMAD_WRITE_GOLDEN").is_some() {
            std::fs::write(&path, encode(rgb, w, h)).expect("write the golden");
            return;
        }
        let keep = || {
            let dir = root.join("../target/lnomad-golden");
            std::fs::create_dir_all(&dir).expect("the keep dir");
            let kept = dir.join(name);
            std::fs::write(&kept, encode(rgb, w, h)).expect("write");
            kept
        };
        let Ok(bytes) = std::fs::read(&path) else {
            panic!("no golden {name}; this run's is {}", keep().display());
        };
        let mut reader = png::Decoder::new(std::io::Cursor::new(bytes))
            .read_info()
            .expect("png");
        let mut want = vec![0; reader.output_buffer_size().expect("size")];
        reader.next_frame(&mut want).expect("frame");
        if want != rgb {
            let moved = want.iter().zip(rgb).filter(|(a, b)| a != b).count();
            panic!(
                "{name} moved in {moved} bytes; this run's is {}",
                keep().display()
            );
        }
    }

    /// The README in the window, 1280 by 800, in each theme: pixel for
    /// pixel the goldens.
    #[test]
    fn the_readme_is_the_golden_in_both_themes() {
        let (w, h) = (WINDOW.0 as u32, WINDOW.1 as u32);
        same_as("gui-page-light.png", &shoot(&readme(Theme::Light)), w, h);
        same_as("gui-page-dark.png", &shoot(&readme(Theme::Dark)), w, h);
    }

    /// Where on the window the first cell of link `n` is, from the
    /// scene's runs on the cell grid; its line and its link.
    fn first_link(window: &Window) -> (iced::Point, usize) {
        let scene = window.model.scene();
        for (row, line) in scene.page.lines.iter().enumerate() {
            let mut col = 0usize;
            for run in &line.runs {
                if let Some(n) = run.link {
                    let x = PAGE_PAD + (col as f32 + 0.5) * CELL_W;
                    let y = BAR + ADDRESS_ROW + PAGE_PAD + (row as f32 + 0.5) * CELL_H;
                    return (iced::Point::new(x, y), n);
                }
                col += UnicodeWidthStr::width(run.text.as_str());
            }
        }
        panic!("the README's window holds no link");
    }

    /// A click on a link run raises `Activate(Link(n))`, and folded, it
    /// navigates there; a click on the page beside it raises nothing.
    #[test]
    fn a_click_on_a_link_activates_it() {
        let mut window = readme(Theme::Light);
        let (at, n) = first_link(&window);
        let clicked =
            leviculum_ui_iced::headless::click(window.view(), at, WINDOW.0 as u32, WINDOW.1 as u32)
                .expect("the software renderer");
        assert!(
            matches!(
                clicked.as_slice(),
                [Message::Intent(Intent::Activate(NodeId::Link(got)))] if *got == n
            ),
            "{clicked:?}"
        );
        let blank = iced::Point::new(WINDOW.0 - 2.0 * PAGE_PAD, at.y);
        let beside = leviculum_ui_iced::headless::click(
            window.view(),
            blank,
            WINDOW.0 as u32,
            WINDOW.1 as u32,
        )
        .expect("the software renderer");
        assert!(beside.is_empty(), "{beside:?}");
        let before = window.model.pending.clone();
        let _ = window.update(clicked.into_iter().next().expect("one message"));
        assert!(before.is_none());
        assert!(
            window.model.pending.is_some() || window.model.toast.is_some(),
            "a followed link loads or says why it cannot"
        );
    }

    /// The window lays the README out on the cells its page holds: at
    /// 1280 by 800, columns and rows the model's size says.
    #[test]
    fn the_page_is_laid_out_on_the_windows_cells() {
        let window = readme(Theme::Light);
        let (cols, rows) = cells(WINDOW.0, WINDOW.1);
        assert_eq!(window.model.size, (cols, rows));
        let scene = window.model.scene();
        assert_eq!(scene.page.lines.len(), window.model.viewport());
        let widest = scene
            .page
            .lines
            .iter()
            .map(|l| {
                l.runs
                    .iter()
                    .map(|r| UnicodeWidthStr::width(r.text.as_str()))
                    .sum::<usize>()
            })
            .max()
            .unwrap_or(0);
        assert!(widest <= usize::from(cols - SCROLLBAR_COLS), "{widest}");
    }

    /// Drawing the README at 1280 by 800, view and paint, the window's
    /// frame: measured over 10 frames and printed; held well under a
    /// second even in the unoptimised test profile.
    #[test]
    fn a_readme_frame_is_drawn_in_well_under_a_second() {
        let window = readme(Theme::Light);
        let _ = shoot(&window);
        let frames = 10;
        let start = Instant::now();
        for _ in 0..frames {
            let _ = shoot(&window);
        }
        let each = start.elapsed() / frames;
        println!("README frame at 1280x800: {each:?}");
        assert!(each < Duration::from_secs(1), "{each:?}");
    }

    /// The name search's results for "marcel" over three sites (the
    /// scene golden's, `tests/scene_golden.rs`) as the window's sheet:
    /// each of the three items is a row a click opens, `Activate(Name(n))`.
    #[test]
    fn the_name_results_are_a_sheet_of_rows_a_click_opens() {
        load_mono();
        let now = 10_000;
        let mut model = Model::from_document(&parse(""), 79, "", (80, 24));
        model.clock = Some(now);
        for (dest, name, hops, seen) in [
            ([0x9c; 16], "Marcel", 1, now - 30),
            ([0xfa; 16], "Marcel", 3, now - 7200),
            ([0x17; 16], "Marcel Solar", 2, now - 600),
        ] {
            model
                .node_registry
                .upsert(dest, Some(name.to_string()), Some(hops), seen);
        }
        update(
            &mut model,
            AppEvent::Intent(Intent::FindName {
                query: "marcel".to_string(),
            }),
        );
        let window = Window::new(model, None);
        assert!(window.model.scene().names.is_some(), "the list is open");
        let x = WINDOW.0 - SHEET_WIDTH / 2.0;
        let mut opened = Vec::new();
        let mut y = BAR + ADDRESS_ROW;
        while y < WINDOW.1 - STATUS_ROW {
            let got = leviculum_ui_iced::headless::click(
                window.view(),
                iced::Point::new(x, y),
                WINDOW.0 as u32,
                WINDOW.1 as u32,
            )
            .expect("the software renderer");
            for message in got {
                if let Message::Intent(Intent::Activate(NodeId::Name(n))) = message {
                    if !opened.contains(&n) {
                        opened.push(n);
                    }
                }
            }
            y += 4.0;
        }
        assert_eq!(opened.len(), 3, "{opened:?}");
    }

    /// The xterm table: the cube's corners and the grey ramp's ends.
    #[test]
    fn an_index_is_the_xterm_colour() {
        assert_eq!(indexed(16), (0, 0, 0));
        assert_eq!(indexed(231), (255, 255, 255));
        assert_eq!(indexed(196), (255, 0, 0));
        assert_eq!(indexed(232), (8, 8, 8));
        assert_eq!(indexed(255), (238, 238, 238));
    }
}
