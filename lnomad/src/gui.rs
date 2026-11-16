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
//! and which answers with outcomes the window folds into the model.
//!
//! A page's pictures are fetched one after another by the queue the
//! terminal runs ([`crate::transfer::spawn_image_queue`]), decoded by the
//! same `image` crate and drawn in the rows the layout reserves under their
//! link, at their natural size on the cell grid and never enlarged; a
//! payload is kept in the same byte-bounded cache, so a revisit costs no
//! airtime.
//!
//! A form field is a box in the terminal's chrome colours, reversed while
//! focused, with a block caret on its editor's cell; a click on it is
//! `Activate(Field(n))` and a key the window's widgets leave types into it
//! through the rows table, as in the terminal.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use iced::widget::text::Span;
use iced::widget::{
    column, container, image, mouse_area, opaque, pin, rich_text, row, scrollable, stack, text,
    Space,
};
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
    show_start_screen, split_path_anchor, update, AppEvent, Effect, HistoryAction, Intent, Model,
    MouseEvent, MouseEventKind, Pending, ToastKind,
};
use crate::browser::BrowserOptions;
use crate::discovery::DiscoveredNode;
use crate::fetch::Session;
use crate::image::SlotState;
use crate::image_cache::ImageCache;
use crate::theme::Theme;
use crate::transfer::{
    close_session, open_saved, save_picture, say_saved, spawn_discovery, spawn_download,
    spawn_fetch, spawn_image_queue, FetchOutcome, ImageOutcome, ImageResult, LoadResult,
};
use crate::url::Target;

/// The page's face: Fira Mono, embedded so a page and its golden agree on
/// every host (`lnomad/fonts/OFL.txt`).
pub const MONO_BYTES: &[u8] = include_bytes!("../fonts/FiraMono-Medium.ttf");

/// Its bold, the same advance on the same grid, for a run the page sets
/// bold. Fira Mono has no italic, so an italic run stays upright: a face
/// asked for that the family lacks falls back to the proportional sans,
/// off the grid.
pub const MONO_BOLD_BYTES: &[u8] = include_bytes!("../fonts/FiraMono-Bold.ttf");

/// The page's face as iced names it: the family, at the one weight the
/// file holds.
pub const MONO: Font = Font {
    family: iced::font::Family::Name("Fira Mono"),
    weight: iced::font::Weight::Medium,
    ..Font::DEFAULT
};

/// The bold face as iced names it.
pub const MONO_BOLD: Font = Font {
    family: iced::font::Family::Name("Fira Mono"),
    weight: iced::font::Weight::Bold,
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

/// The window's grounds and words in the theme the model is in: the
/// suite's chrome ([`leviculum_ui::pixel`]), the one Loquax's window
/// draws in. The page's own colours are the document's.
pub fn palette(theme: Theme) -> Palette {
    Palette::of(&leviculum_ui::pixel::chrome(theme))
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
    /// The pointer over the page, where on it; `None` once it left.
    Pointer(Option<iced::Point>),
    /// The window has this logical size.
    Resized(Size),
    /// A transfer's outcome.
    Outcome(Outcome),
    /// A frame is drawn: the spinner's and the toast's clock.
    Frame(Instant),
    /// The desktop's light or dark, asked at the start and on each change.
    SystemTheme(iced::theme::Mode),
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
    /// A picture of the page: its transfer started, its bytes, or why not.
    Picture {
        link: usize,
        target: Target,
        result: Picture,
    },
}

/// What the image queue says about one picture.
#[derive(Clone, Debug)]
pub enum Picture {
    Started,
    Bytes(Vec<u8>),
    Failed(String),
}

/// A picture the window holds for the current page: the payload, kept for
/// saving and a resize, its pixels as iced draws them, its size.
struct Held {
    name: String,
    bytes: Vec<u8>,
    handle: image::Handle,
    px: (u32, u32),
    saved: Option<PathBuf>,
}

/// The window's half of a page's pictures: the current page's, decoded
/// and held by link, and the payloads held across pages, bounded by bytes
/// as the terminal's. Kept out of [`Model`] as the terminal keeps its
/// `ImageStore` out: the model holds the slots, this the pixels. The
/// standalone window holds one beside its model; Loquax's window holds one
/// beside the browser its Browse tab hosts (1369), so both draw a page's
/// pictures with this code.
#[derive(Default)]
pub struct Pictures {
    held: HashMap<usize, Held>,
    cache: ImageCache,
}

impl Pictures {
    /// Drop the current page's pictures; the cache outlives the page.
    pub fn clear(&mut self) {
        self.held.clear();
    }

    /// Whether the picture of `link` is held.
    pub fn holds(&self, link: usize) -> bool {
        self.held.contains_key(&link)
    }

    /// What the image queue said of one picture, as the event the model
    /// folds: a payload is cached, decoded and held first.
    pub fn arrived(
        &mut self,
        model: &Model,
        link: usize,
        target: Target,
        result: Picture,
    ) -> AppEvent {
        match result {
            Picture::Started => AppEvent::ImageLoading { link },
            Picture::Failed(reason) => AppEvent::ImageFailed { link, reason },
            Picture::Bytes(bytes) => {
                // Cached before it is drawn, so it survives the page:
                // the airtime is already spent either way.
                self.cache.insert(target, bytes.clone());
                self.accept(model, link, bytes)
            }
        }
    }

    /// A page's picture requests split into the events of the ones the
    /// cache holds, drawn at once as a fetched one would be, and the ones
    /// still to fetch. The current page's pictures are dropped first.
    pub fn cached(
        &mut self,
        model: &Model,
        requests: Vec<crate::app::ImageRequest>,
    ) -> (Vec<AppEvent>, Vec<crate::app::ImageRequest>) {
        self.held.clear();
        let mut events = Vec::new();
        let mut pending = Vec::new();
        for request in requests {
            match self.cache.get(&request.target).map(<[u8]>::to_vec) {
                Some(bytes) => events.push(self.accept(model, request.link, bytes)),
                None => pending.push(request),
            }
        }
        (events, pending)
    }

    /// Take a picture's payload: decoded, held and fitted to the page, or a
    /// line saying why not, as the terminal tells it.
    fn accept(&mut self, model: &Model, link: usize, bytes: Vec<u8>) -> AppEvent {
        let Some(probe) = crate::image::probe(&bytes) else {
            return AppEvent::ImageText {
                link,
                probe: None,
                reason: "not a format this browser decodes".to_string(),
            };
        };
        let rgba = match crate::image::decode(&bytes) {
            Ok(decoded) => decoded.into_rgba8(),
            Err(reason) => {
                return AppEvent::ImageText {
                    link,
                    probe: Some(probe),
                    reason,
                }
            }
        };
        let px = rgba.dimensions();
        let name = model
            .image_slot(link)
            .map_or_else(|| "image".to_string(), |slot| slot.name.clone());
        self.held.insert(
            link,
            Held {
                name,
                bytes,
                handle: image::Handle::from_rgba(px.0, px.1, rgba.into_raw()),
                px,
                saved: None,
            },
        );
        AppEvent::ImageReady {
            link,
            cells: fit(model, px),
            probe,
        }
    }

    /// Every drawn picture fitted again after a resize, as the events that
    /// say so, so the rows it reserves follow the page's width.
    pub fn refit(&self, model: &Model) -> Vec<AppEvent> {
        model
            .images
            .iter()
            .filter_map(|slot| match slot.state {
                SlotState::Ready { probe, .. } => Some((slot.link, probe)),
                _ => None,
            })
            .filter_map(|(link, probe)| {
                let px = self.held.get(&link)?.px;
                Some(AppEvent::ImageReady {
                    link,
                    cells: fit(model, px),
                    probe,
                })
            })
            .collect()
    }

    /// A held picture saved once into the download directory.
    pub fn save(&mut self, link: usize) -> Result<(PathBuf, usize), Text> {
        let Some(held) = self.held.get_mut(&link) else {
            return Err(Text::key(StrId::ImageNotArrived));
        };
        save_picture(&held.name, &held.bytes, &mut held.saved)
    }
}

/// The cells a picture of `px` pixels takes on the page: its natural size
/// on the cell grid, shrunk to the page's width and the picture row
/// ceiling, never enlarged.
fn fit(model: &Model, px: (u32, u32)) -> (u16, u16) {
    crate::image::fit_cells(
        px,
        (CELL_W.round() as u16, CELL_H as u16),
        model.width.min(usize::from(u16::MAX)) as u16,
        crate::image::picture_rows(model.viewport()),
    )
}

/// The wheel's turn of `lines` lines down (negative up) as the terminal's
/// wheel notches the model folds, [`WHEEL_LINES`] a notch.
pub fn wheel_mice(lines: f32) -> Vec<MouseEvent> {
    let kind = match lines > 0.0 {
        true => MouseEventKind::ScrollDown,
        false => MouseEventKind::ScrollUp,
    };
    let notches = (lines.abs() / WHEEL_LINES as f32).ceil().max(1.0) as usize;
    (0..notches)
        .map(|_| MouseEvent {
            kind,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        })
        .collect()
}

/// The pointer over the page as the terminal's mouse reports it: the
/// page's cell under it, below the top bar's row. Off the page, the top
/// bar's row, where no link is.
pub fn pointer_mouse(at: Option<iced::Point>) -> MouseEvent {
    let (column, row) = at
        .filter(|at| at.x >= PAGE_PAD && at.y >= PAGE_PAD)
        .map_or((0, 0), |at| {
            (
                ((at.x - PAGE_PAD) / CELL_W) as u16,
                (((at.y - PAGE_PAD) / CELL_H) as u16)
                    .saturating_add(crate::app::model::TOPBAR_ROWS),
            )
        });
    MouseEvent {
        kind: MouseEventKind::Moved,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

/// The browser's window: the model, and where effects that leave the
/// process go.
pub struct Window {
    pub model: Model,
    jobs: Option<tokio_mpsc::UnboundedSender<Effect>>,
    /// When the model last ticked.
    ticked: Option<Instant>,
    /// Under `--theme auto`, the theme the desktop last set: the window
    /// follows the desktop until the reader flips the theme away from it.
    follows: Option<Theme>,
    /// The current page's pictures and the payloads held across pages.
    pictures: Pictures,
}

impl Window {
    /// A window over `model`, `jobs` the transfers' inbox; none in a test,
    /// where an effect that would leave the process is dropped.
    pub fn new(mut model: Model, jobs: Option<tokio_mpsc::UnboundedSender<Effect>>) -> Self {
        let (cols, rows) = cells(WINDOW.0, WINDOW.1);
        update(&mut model, AppEvent::Resize(cols, rows));
        Window {
            model,
            jobs,
            ticked: None,
            follows: None,
            pictures: Pictures::default(),
        }
    }

    /// The window following the desktop's light or dark, as `--theme
    /// auto` asks: the model's theme until the desktop answers.
    pub fn following(mut self) -> Self {
        self.follows = Some(self.model.theme);
        self
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
            Message::Address(value) => crate::app::type_address(&mut self.model, &value),
            Message::AddressGo => apply(&mut self.model, Intent::Go { url: None }),
            Message::Key(event) => match leviculum_ui_iced::key::chord(&event) {
                Some(key) => update(&mut self.model, AppEvent::Key(key)),
                None => Vec::new(),
            },
            Message::Wheel(lines) => {
                let mut effects = Vec::new();
                for mouse in wheel_mice(lines) {
                    effects.extend(update(&mut self.model, AppEvent::Mouse(mouse)));
                }
                effects
            }
            Message::Pointer(at) => update(&mut self.model, AppEvent::Mouse(pointer_mouse(at))),
            Message::Resized(size) => {
                let (cols, rows) = cells(size.width, size.height);
                let mut effects = update(&mut self.model, AppEvent::Resize(cols, rows));
                for event in self.pictures.refit(&self.model) {
                    effects.extend(update(&mut self.model, event));
                }
                effects
            }
            Message::Outcome(outcome) => self.fold(outcome),
            Message::SystemTheme(mode) => {
                let desktop = match mode {
                    iced::theme::Mode::Light => Some(Theme::Light),
                    iced::theme::Mode::Dark => Some(Theme::Dark),
                    iced::theme::Mode::None => None,
                };
                // Only while the reader has not flipped the theme away from
                // what the desktop last set.
                if let (Some(desktop), Some(last)) = (desktop, self.follows) {
                    if self.model.theme == last {
                        self.model.theme = desktop;
                        self.follows = Some(desktop);
                    }
                }
                Vec::new()
            }
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
            Outcome::Picture {
                link,
                target,
                result,
            } => {
                let event = self.pictures.arrived(&self.model, link, target, result);
                update(&mut self.model, event)
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
                // Never raised here: the window is not hosted by a messenger.
                Effect::Write(_) => {}
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
                Effect::Navigate(target) => {
                    // The pictures of the page being left are of no use.
                    self.pictures.clear();
                    self.send(Effect::Navigate(target));
                }
                Effect::FetchImages(requests) => {
                    let (events, pending) = self.pictures.cached(&self.model, requests);
                    for event in events {
                        update(&mut self.model, event);
                    }
                    // Even an empty list goes: it stops the last page's queue.
                    self.send(Effect::FetchImages(pending));
                }
                Effect::SaveImage { link } => {
                    let saved = self.pictures.save(link);
                    say_saved(&mut self.model, saved);
                }
                Effect::OpenImage { link } => {
                    let saved = self.pictures.save(link);
                    open_saved(&mut self.model, saved);
                }
                leaving => self.send(leaving),
            }
        }
        Task::batch(tasks)
    }

    /// Hand an effect that leaves the process to the transfers.
    fn send(&self, effect: Effect) {
        if let Some(jobs) = &self.jobs {
            let _ = jobs.send(effect);
        }
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
        if self.follows.is_some() {
            all.push(leviculum_ui_iced::desktop::theme_changes().map(Message::SystemTheme));
        }
        Subscription::batch(all)
    }

    pub fn view(&self) -> Element<'_, Message> {
        view(&self.model, &self.pictures)
    }
}

/// The browser's window drawn from `model`'s scene: the bar, the address,
/// the page with its pictures, a sheet beside it, the status line and the
/// help over all. The standalone window's whole view, and the Browse tab's
/// content in Loquax's window (1369), which hosts the model and keeps its
/// pictures, so the tab is drawn by this code and no second renderer.
pub fn view<'a>(model: &Model, pictures: &Pictures) -> Element<'a, Message> {
    let scene = model.scene();
    let p = palette(model.theme);
    let lang = model.lang;
    let bar = top_bar(&scene, lang, p);
    let address = address_row(&scene.address, &scene, lang, p);
    let page = page_view(&scene.page, model, &pictures.held, p);
    let page: Element<'a, Message> = mouse_area(page)
        .on_scroll(wheel)
        .on_move(|at| Message::Pointer(Some(at)))
        .on_exit(Message::Pointer(None))
        .into();
    let sheet = scene
        .names
        .as_ref()
        .or(scene.places.as_ref())
        .map(|sheet| sheet_view(sheet, lang, p));
    let middle: Element<'a, Message> = match sheet {
        Some(sheet) => row![page, sheet].height(Length::Fill).into(),
        None => page,
    };
    let status = status_row(&scene, lang, model.theme, p);
    let window: Element<'a, Message> = container(column![bar, address, middle, status])
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_: &IcedTheme| container::Style {
            background: Some(Background::Color(p.background)),
            ..container::Style::default()
        })
        .into();
    match scene.help {
        true => stack![window, help_layer(model, p)].into(),
        false => window,
    }
}

/// The help over the window: the shared overlay on the page's cell grid,
/// laid out over the model's cells as the terminal lays it over its
/// screen, so the key handler's scroll bounds are what it shows. The wheel
/// scrolls it and a click closes it, as in the terminal; nothing beneath
/// takes either.
fn help_layer<'a>(model: &Model, p: Palette) -> Element<'a, Message> {
    let (cols, rows) = model.size;
    let grid = leviculum_ui_iced::help::Grid {
        regular: MONO,
        bold: MONO_BOLD,
        size: PAGE_TEXT,
        cell_w: CELL_W,
        cell_h: CELL_H,
    };
    let overlay = leviculum_ui_iced::help::overlay(
        &crate::app::help_groups_in(model.lang),
        StrId::HelpTitle.text(model.lang).trim(),
        model.help_scroll,
        leviculum_ui::Rect::new(0, 0, cols, rows),
        grid,
        p,
    );
    opaque(
        mouse_area(container(overlay).center(Length::Fill))
            .on_press(Message::Intent(Intent::Dismiss))
            .on_scroll(wheel),
    )
}

/// The wheel's delta as lines down.
fn wheel(delta: mouse::ScrollDelta) -> Message {
    Message::Wheel(match delta {
        mouse::ScrollDelta::Lines { y, .. } => -y * WHEEL_LINES as f32,
        mouse::ScrollDelta::Pixels { y, .. } => -y / CELL_H,
    })
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

/// A chip's words on the bar's ground, the address field's: the light
/// theme's panel is its screen's white.
fn pill<'a>(words: String, p: Palette) -> Element<'a, Message> {
    container(text(words).size(13).color(p.text_secondary))
        .padding([3, 10])
        .style(move |_: &IcedTheme| container::Style {
            background: Some(Background::Color(p.bar)),
            border: border::rounded(12),
            ..container::Style::default()
        })
        .into()
}

/// What a press on a page run activates: its link, or its form field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Link(usize),
    Field(usize),
}

impl Hit {
    fn intent(self) -> Intent {
        Intent::Activate(match self {
            Hit::Link(n) => NodeId::Link(n),
            Hit::Field(n) => NodeId::Field(n),
        })
    }
}

/// What the page view needs beside the runs: the palette, the focused link
/// or field, the field boxes' colours and the caret's cell in the focused
/// text field.
#[derive(Clone, Copy)]
struct Ground {
    p: Palette,
    focus: Option<crate::app::Focus>,
    /// A field box's words and ground, the terminal's chrome pair.
    field: (Color, Color),
    /// The focused text field and its caret's cell, counted from the box's
    /// first cell.
    caret: Option<(usize, usize)>,
}

/// The caret of the focused text field: the field and the cell, from the
/// box's first, its editor's column less the box's scroll, held inside the
/// box, as the terminal places its cursor.
fn caret(model: &Model, focus: Option<crate::app::Focus>) -> Option<(usize, usize)> {
    let Some(crate::app::Focus::Field(n)) = focus else {
        return None;
    };
    let field = crate::app::model::visible_fields(model)
        .into_iter()
        .find(|f| f.index == n && f.kind == leviculum_micron::FieldKind::Text)?;
    let edit = model.field_state.get(n.checked_sub(1)?)?;
    let w = usize::from(field.col_end.saturating_sub(field.col_start));
    let scroll = crate::app::input::visual_scroll(&edit.input, w.saturating_sub(1));
    let at = crate::app::input::visual_cursor(&edit.input).saturating_sub(scroll);
    Some((n, at.min(w.saturating_sub(1))))
}

/// One run as a span: its colours, weight, underline, the link or field it
/// is part of; the focused link on the selection's ground, a field in its
/// box (reversed while focused), a search match on the accent's.
fn span<'a>(text: String, run: &Run, g: &Ground, caret: bool) -> Span<'a, Hit, Font> {
    let style = run.style;
    let p = g.p;
    let (mut fg, mut bg) = (style.fg.map(ink), style.bg.map(ink));
    if style.reverse {
        (fg, bg) = (Some(bg.unwrap_or(p.background)), Some(fg.unwrap_or(p.text)));
    }
    if let Some(n) = run.link {
        if g.focus == Some(crate::app::Focus::Link(n)) {
            bg = Some(p.selected_row);
        }
    }
    if let Some(n) = run.field {
        let (words, ground) = g.field;
        // The focused field reversed, its caret's cell back in the box's
        // colours: a block caret.
        let reversed = g.focus == Some(crate::app::Focus::Field(n)) && !caret;
        (fg, bg) = match reversed {
            true => (Some(ground), Some(words)),
            false => (Some(words), Some(ground)),
        };
    }
    match run.matched {
        Some(true) => bg = Some(p.accent),
        Some(false) => bg = Some(p.panel),
        None => {}
    }
    let font = match style.bold {
        true => MONO_BOLD,
        false => MONO,
    };
    let mut span = Span::new(text)
        .font(font)
        .color(fg.unwrap_or(p.text))
        .underline(style.underline);
    if let Some(bg) = bg {
        span = span.background(Background::Color(bg));
    }
    let hit = match (run.link, run.field) {
        (Some(n), _) => Some(Hit::Link(n)),
        (None, Some(n)) => Some(Hit::Field(n)),
        (None, None) => None,
    };
    span.link_maybe(hit)
}

/// `text` cut at cell `at`: the characters before it, the one on it and
/// those after.
fn cut_at(text: &str, at: usize) -> (String, String, String) {
    let (mut before, mut on, mut after) = (String::new(), String::new(), String::new());
    let mut col = 0;
    for c in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if !on.is_empty() {
            after.push(c);
        } else if col + w <= at {
            before.push(c);
        } else {
            on.push(c);
        }
        col += w;
    }
    (before, on, after)
}

/// A line's runs as spans, the caret's run split around its cell.
fn line_spans<'a>(line: &crate::app::scene::PageLine, g: &Ground) -> Vec<Span<'a, Hit, Font>> {
    let mut spans = Vec::new();
    // Where in its box each field's next run starts.
    let mut into = 0usize;
    let mut field = None;
    for run in &line.runs {
        if run.field != field {
            (field, into) = (run.field, 0);
        }
        let width = unicode_width::UnicodeWidthStr::width(run.text.as_str());
        match g.caret {
            Some((n, at)) if run.field == Some(n) && (into..into + width).contains(&at) => {
                let (before, on, after) = cut_at(&run.text, at - into);
                for (part, caret) in [(before, false), (on, true), (after, false)] {
                    if !part.is_empty() {
                        spans.push(span(part, run, g, caret));
                    }
                }
            }
            _ => spans.push(span(run.text.clone(), run, g, false)),
        }
        into += width;
    }
    spans
}

/// The page: its window of lines on the cell grid, one rich text per
/// line, a click on a link run its activation, on a field run the field's.
fn page_view<'a>(
    page: &Page,
    model: &Model,
    pictures: &HashMap<usize, Held>,
    p: Palette,
) -> Element<'a, Message> {
    let words = model.theme.chrome_fg();
    let ground = model.theme.chrome_bg();
    let g = Ground {
        p,
        focus: page.focus,
        field: (
            Color::from_rgb8(words.0, words.1, words.2),
            Color::from_rgb8(ground.0, ground.1, ground.2),
        ),
        caret: caret(model, page.focus),
    };
    let lines = page.lines.iter().fold(column![], |lines, line| {
        let spans = line_spans(line, &g);
        let line: Element<'a, Message> = match spans.is_empty() {
            true => Space::new().height(CELL_H).into(),
            false => rich_text(spans)
                .size(PAGE_TEXT)
                .line_height(iced::Pixels(CELL_H))
                .font(MONO)
                .wrapping(text::Wrapping::None)
                .on_link_click(|hit: Hit| Message::Intent(hit.intent()))
                .into(),
        };
        lines.push(container(line).height(CELL_H))
    });
    let lines: Element<'a, Message> = match model.images.is_empty() {
        true => lines.into(),
        false => stack![lines, picture_layer(model, pictures, p)]
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
    };
    container(lines)
        .padding(PAGE_PAD)
        .width(Length::Fill)
        .height(Length::Fill)
        .clip(true)
        .into()
}

/// The page's pictures over the rows the layout reserved under their
/// links, and a line of status for each that is not a picture (queued,
/// loading, failed, not decodable), as the terminal draws them. A picture
/// half-scrolled off the top is drawn from its middle; the page clips it.
fn picture_layer<'a>(
    model: &Model,
    pictures: &HashMap<usize, Held>,
    p: Palette,
) -> Element<'a, Message> {
    let viewport = model.viewport() as i64;
    let mut layer = stack![].width(Length::Fill).height(Length::Fill);
    for slot in &model.images {
        let Some(link) = model.links.iter().find(|l| l.index == slot.link) else {
            continue;
        };
        // The reserved rows sit directly under the link's line.
        let y = link.line as i64 + 1 - model.scroll as i64;
        let x = link.col_start as f32 * CELL_W;
        let at = |item: Element<'a, Message>| pin(item).x(x).y(y as f32 * CELL_H);
        match (&slot.state, pictures.get(&slot.link)) {
            (SlotState::Ready { cells, .. }, Some(held)) => {
                if y + i64::from(cells.1) <= 0 || y >= viewport {
                    continue;
                }
                // Flush left, as the terminal draws it: the widget is the
                // picture's own size fitted into its cells, not the cells,
                // in which iced would centre it.
                let size = iced::ContentFit::ScaleDown.fit(
                    Size::new(held.px.0 as f32, held.px.1 as f32),
                    Size::new(f32::from(cells.0) * CELL_W, f32::from(cells.1) * CELL_H),
                );
                let picture = image(held.handle.clone())
                    .width(size.width)
                    .height(size.height)
                    .content_fit(iced::ContentFit::ScaleDown);
                layer = layer.push(at(picture.into()));
            }
            (state, _) => {
                if y < 0 || y >= viewport {
                    continue;
                }
                let words = crate::image::status_text(&slot.name, state).resolve(model.lang);
                let line = text(words)
                    .font(MONO)
                    .size(PAGE_TEXT)
                    .line_height(iced::Pixels(CELL_H))
                    .color(p.text_secondary)
                    .wrapping(text::Wrapping::None);
                layer = layer.push(at(line.into()));
            }
        }
    }
    layer.into()
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
    theme: Theme,
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
            ToastKind::Error => {
                leviculum_ui_iced::paint::colour(leviculum_ui::pixel::chrome(theme).failed_on_panel)
            }
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
    system.load_font(Cow::Borrowed(MONO_BOLD_BYTES));
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
    let (image_tx, mut image_rx) = tokio_mpsc::unbounded_channel::<ImageOutcome>();
    let mut pictures: Option<tokio::task::JoinHandle<()>> = None;
    let mut picture_generation: u64 = 0;
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
                    stop_pictures(&mut pictures, &mut picture_generation);
                    spawn_fetch(&mut inflight, &mut generation, &session, &tx, timeout, target);
                }
                Some(Effect::FetchImages(requests)) => {
                    let current = stop_pictures(&mut pictures, &mut picture_generation);
                    if !requests.is_empty() {
                        pictures = Some(spawn_image_queue(
                            &session, &image_tx, timeout, requests, current,
                        ));
                    }
                }
                Some(Effect::Download(target)) => {
                    spawn_download(&mut inflight, &mut generation, &session, &tx, timeout, target);
                }
                Some(Effect::Cancel) => {
                    if let Some(handle) = inflight.take() {
                        handle.abort();
                    }
                    generation = generation.wrapping_add(1);
                    stop_pictures(&mut pictures, &mut picture_generation);
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
            Some(picture) = image_rx.recv() => {
                // A stale generation belongs to a page the reader has left.
                if picture.generation == picture_generation {
                    let result = match picture.result {
                        ImageResult::Started => Picture::Started,
                        ImageResult::Bytes(bytes) => Picture::Bytes(bytes),
                        ImageResult::Failed(reason) => Picture::Failed(reason),
                    };
                    let _ = out.unbounded_send(Outcome::Picture {
                        link: picture.link,
                        target: picture.target,
                        result,
                    });
                }
            }
        }
    }
    if let Some(handle) = inflight.take() {
        handle.abort();
        let _ = handle.await;
    }
    if let Some(handle) = pictures.take() {
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

/// Stop the last page's picture queue, whose pictures are nobody's now:
/// the task aborted and the generation moved on, which is returned.
fn stop_pictures(task: &mut Option<tokio::task::JoinHandle<()>>, generation: &mut u64) -> u64 {
    if let Some(handle) = task.take() {
        handle.abort();
    }
    *generation = generation.wrapping_add(1);
    *generation
}

/// Open the window over `session` and run it until it is closed: the
/// model as the terminal sets it up, the transfers on the runtime the
/// session was made on, the window on this thread.
/// `theme` is the window's theme, or `None` for the desktop's (`--theme
/// auto`), dark until the desktop says.
pub async fn run_gui(
    session: Session,
    initial: Option<Target>,
    opts: BrowserOptions,
    theme: Option<Theme>,
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
        no_images: opts.no_images,
        depth: opts.depth,
        theme: theme.unwrap_or(Theme::Dark),
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
    let slot = Mutex::new(Some((model, theme.is_none(), jobs_tx, out_rx)));
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
        .font(MONO_BOLD_BYTES)
        .window(iced::window::Settings {
            icon: leviculum_ui_iced::window_icon(),
            ..iced::window::Settings::default()
        })
        .window_size(Size::new(WINDOW.0, WINDOW.1))
        .run()
    });
    // The window dropped its inbox: the transfers close the session.
    let _ = driver.await;
    result
}

/// The model, whether the window follows the desktop's theme, the
/// transfers' inbox and their answers.
type Handed = (
    Model,
    bool,
    tokio_mpsc::UnboundedSender<Effect>,
    UnboundedReceiver<Outcome>,
);

fn boot(handed: Option<Handed>) -> (Window, Task<Message>) {
    match handed {
        Some((model, follow, jobs, outcomes)) => {
            let window = Window::new(model, Some(jobs));
            let outcomes = Task::run(outcomes, Message::Outcome);
            match follow {
                true => (
                    window.following(),
                    Task::batch([
                        outcomes,
                        leviculum_ui_iced::desktop::theme().map(Message::SystemTheme),
                    ]),
                ),
                false => (window, outcomes),
            }
        }
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

    /// The messages a click at `at` publishes, less the pointer's move
    /// the click begins with.
    fn pressed(window: &Window, at: iced::Point) -> Vec<Message> {
        leviculum_ui_iced::headless::click(window.view(), at, WINDOW.0 as u32, WINDOW.1 as u32)
            .expect("the software renderer")
            .into_iter()
            .filter(|m| !matches!(m, Message::Pointer(_)))
            .collect()
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
        let clicked = pressed(&window, at);
        assert!(
            matches!(
                clicked.as_slice(),
                [Message::Intent(Intent::Activate(NodeId::Link(got)))] if *got == n
            ),
            "{clicked:?}"
        );
        let blank = iced::Point::new(WINDOW.0 - 2.0 * PAGE_PAD, at.y);
        let beside = pressed(&window, blank);
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
    ///
    /// The bound is on the CPU time of the drawing thread (the render is
    /// on it), not the wall clock: what it catches is a frame that does far
    /// more work than its release time (16 ms; about 300 ms here), and a
    /// quota or a loaded host stretches the wall clock without any (1.003 s
    /// once at load 33, 1310). Where the scheduler's count is not readable
    /// it falls back to the wall clock.
    #[test]
    fn a_readme_frame_is_drawn_in_well_under_a_second() {
        let window = readme(Theme::Light);
        let _ = shoot(&window);
        let frames = 10;
        let start = Instant::now();
        let cpu_start = thread_cpu();
        for _ in 0..frames {
            let _ = shoot(&window);
        }
        let wall = start.elapsed() / frames;
        let cpu = thread_cpu()
            .zip(cpu_start)
            .map(|(end, start)| (end - start) / frames);
        println!("README frame at 1280x800: {cpu:?} CPU, {wall:?} wall");
        let each = cpu.unwrap_or(wall);
        assert!(each < Duration::from_secs(1), "{each:?}");
    }

    /// This thread's time on a CPU so far, the scheduler's own count in
    /// nanoseconds (`/proc/thread-self/schedstat`, Linux).
    fn thread_cpu() -> Option<Duration> {
        let stat = std::fs::read_to_string("/proc/thread-self/schedstat").ok()?;
        let ns = stat.split_whitespace().next()?.parse().ok()?;
        Some(Duration::from_nanos(ns))
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

    /// A page with a text field and a checkbox, as a load leaves it, in
    /// the window.
    fn form() -> Window {
        load_mono();
        let src = "`!Sign in`!\nName: `<12|name`bo>\nAgree: `<?|agree`Agree>";
        let (cols, rows) = cells(WINDOW.0, WINDOW.1);
        let mut model = Model::from_document(
            &parse(src),
            crate::app::model::content_width(cols),
            "Form",
            (cols, rows),
        );
        let target = crate::url::parse_url(&format!("{NODE}:/page/form.mu"), None).expect("url");
        model.current_dest = Some(target.dest_hash);
        model.history.visit(target);
        model.theme = Theme::Light;
        Window::new(model, None)
    }

    /// The window's place of cell `col` of the page's line `row`.
    fn cell(col: usize, row: usize) -> iced::Point {
        iced::Point::new(
            PAGE_PAD + (col as f32 + 0.5) * CELL_W,
            BAR + ADDRESS_ROW + PAGE_PAD + (row as f32 + 0.5) * CELL_H,
        )
    }

    fn press(c: char) -> Message {
        let key = keyboard::Key::Character(c.to_string().into());
        Message::Key(keyboard::Event::KeyPressed {
            key: key.clone(),
            modified_key: key,
            physical_key: keyboard::key::Physical::Unidentified(
                keyboard::key::NativeCode::Unidentified,
            ),
            location: keyboard::Location::Standard,
            modifiers: keyboard::Modifiers::empty(),
            text: Some(c.to_string().into()),
            repeat: false,
        })
    }

    /// A click on a field's box raises `Activate(Field(n))`; folded, the
    /// field has the focus and a typed key lands in it, as in the terminal.
    #[test]
    fn a_click_on_a_field_focuses_it_and_keys_type_into_it() {
        let mut window = form();
        let field = crate::app::model::visible_fields(&window.model)[0].clone();
        let row = usize::from(field.row - crate::app::model::TOPBAR_ROWS);
        let at = cell(usize::from(field.col_start) + 1, row);
        let clicked = pressed(&window, at);
        assert!(
            matches!(
                clicked.as_slice(),
                [Message::Intent(Intent::Activate(NodeId::Field(1)))]
            ),
            "{clicked:?}"
        );
        let _ = window.update(clicked.into_iter().next().expect("one message"));
        assert_eq!(window.model.field_focus, Some(1));
        let _ = window.update(press('b'));
        assert_eq!(
            crate::app::input::InputBuffer::value(&window.model.field_state[0].input),
            "bob"
        );
    }

    /// The focused text field is reversed and its caret's cell, after the
    /// typed "bo", is back in the box's colours: three spans, the middle
    /// one the caret.
    #[test]
    fn the_focused_field_is_reversed_around_a_block_caret() {
        let mut window = form();
        let _ = window.update(Message::Intent(Intent::Activate(NodeId::Field(1))));
        let scene = window.model.scene();
        let p = window.palette();
        let g = Ground {
            p,
            focus: scene.page.focus,
            field: (Color::BLACK, Color::WHITE),
            caret: caret(&window.model, scene.page.focus),
        };
        assert_eq!(g.caret, Some((1, 2)), "after \"bo\"");
        let line = scene
            .page
            .lines
            .iter()
            .find(|l| l.runs.iter().any(|r| r.field == Some(1)))
            .expect("the field's line");
        let boxed: Vec<_> = line_spans(line, &g)
            .into_iter()
            .filter(|s| s.link == Some(Hit::Field(1)))
            .collect();
        let grounds: Vec<_> = boxed
            .iter()
            .map(|s| (s.text.to_string(), s.highlight.map(|h| h.background)))
            .collect();
        let reversed = Some(Background::Color(Color::BLACK));
        let plain = Some(Background::Color(Color::WHITE));
        assert_eq!(grounds.len(), 3, "{grounds:?}");
        assert_eq!(grounds[0], ("bo".to_string(), reversed), "{grounds:?}");
        assert_eq!(grounds[1].1, plain, "{grounds:?}");
        assert_eq!(grounds[2].1, reversed, "{grounds:?}");
    }

    /// A bold run is set in the bold face of the same family, a plain one
    /// in the regular: the heading of the form, and its words after it.
    #[test]
    fn a_bold_run_is_set_in_the_bold_face() {
        let window = form();
        let scene = window.model.scene();
        let g = Ground {
            p: window.palette(),
            focus: None,
            field: (Color::BLACK, Color::WHITE),
            caret: None,
        };
        let spans: Vec<_> = scene
            .page
            .lines
            .iter()
            .flat_map(|l| line_spans(l, &g))
            .collect();
        let face = |words: &str| {
            spans
                .iter()
                .find(|s| s.text.contains(words))
                .and_then(|s| s.font)
        };
        assert_eq!(face("Sign in"), Some(MONO_BOLD));
        assert_eq!(face("Name"), Some(MONO));
    }

    /// The form in the window, its field focused: the golden, pixel for
    /// pixel (the boxes, the reversed field, the caret, the bold heading).
    #[test]
    fn the_form_is_the_golden() {
        let mut window = form();
        let _ = window.update(Message::Intent(Intent::Activate(NodeId::Field(1))));
        let (w, h) = (WINDOW.0 as u32, WINDOW.1 as u32);
        same_as("gui-form-light.png", &shoot(&window), w, h);
    }

    /// `?` over the README opens the help in the window: the golden in
    /// the dark theme; a click on it closes it, as in the terminal.
    #[test]
    fn the_help_is_the_golden_and_a_click_closes_it() {
        let mut window = readme(Theme::Dark);
        let _ = window.update(press('?'));
        assert!(window.model.scene().help, "? opens the help");
        let (w, h) = (WINDOW.0 as u32, WINDOW.1 as u32);
        same_as("gui-help-dark.png", &shoot(&window), w, h);
        let clicked = leviculum_ui_iced::headless::click(
            window.view(),
            iced::Point::new(WINDOW.0 / 2.0, WINDOW.1 / 2.0),
            w,
            h,
        )
        .expect("the software renderer");
        assert!(
            matches!(clicked.as_slice(), [Message::Intent(Intent::Dismiss)]),
            "{clicked:?}"
        );
        let _ = window.update(clicked.into_iter().next().expect("one message"));
        assert!(!window.model.scene().help);
    }

    /// Under `--theme auto` the window takes the desktop's light or dark
    /// and each change of it, until the reader flips the theme; without
    /// it the desktop is not asked.
    #[test]
    fn the_window_follows_the_desktop_until_the_reader_flips() {
        let mut window = readme(Theme::Dark).following();
        let _ = window.update(Message::SystemTheme(iced::theme::Mode::Light));
        assert_eq!(window.model.theme, Theme::Light);
        let _ = window.update(Message::SystemTheme(iced::theme::Mode::None));
        assert_eq!(window.model.theme, Theme::Light, "no tone keeps it");
        let _ = window.update(Message::SystemTheme(iced::theme::Mode::Dark));
        assert_eq!(window.model.theme, Theme::Dark);
        let _ = window.update(Message::Intent(Intent::Theme));
        assert_eq!(window.model.theme, Theme::Light, "the reader flips");
        let _ = window.update(Message::SystemTheme(iced::theme::Mode::Dark));
        assert_eq!(window.model.theme, Theme::Light, "the flip holds");

        let mut fixed = readme(Theme::Light);
        let _ = fixed.update(Message::SystemTheme(iced::theme::Mode::Dark));
        assert_eq!(fixed.model.theme, Theme::Light, "--theme light is light");
    }

    /// A blog page with one picture, a standalone link to the node's file
    /// area, the terminal tests' page.
    const PICTURE_PAGE: &str = "\
>>Antennenbau

Der Mast steht.

`[Antenne`:/file/antenne.png]

Text unter dem Bild.
";

    /// A PNG of `w` by `h` pixels, a gradient.
    fn png_bytes(w: u32, h: u32) -> Vec<u8> {
        let mut img = ::image::RgbaImage::new(w, h);
        for (x, y, px) in img.enumerate_pixels_mut() {
            *px = ::image::Rgba([(x * 2 % 256) as u8, (y * 4 % 256) as u8, 200, 255]);
        }
        let mut out = std::io::Cursor::new(Vec::new());
        ::image::DynamicImage::ImageRgba8(img)
            .write_to(&mut out, ::image::ImageFormat::Png)
            .expect("encode png");
        out.into_inner()
    }

    /// The window with the picture page loaded through the ordinary load
    /// path, its effects run: the picture asked for, the request returned.
    fn picture_window() -> (Window, crate::app::ImageRequest) {
        load_mono();
        let mut model = Model {
            current_dest: Some([0xab; 16]),
            theme: Theme::Light,
            ..Model::default()
        };
        model.pending = Some(Pending {
            target: crate::url::parse_url(&format!("{}:/page/blog.mu", "ab".repeat(16)), None)
                .expect("url"),
            action: HistoryAction::Push,
        });
        let mut window = Window::new(model, None);
        let effects = update(
            &mut window.model,
            AppEvent::PageLoaded {
                doc: parse(PICTURE_PAGE),
                title: "Blog".to_string(),
            },
        );
        let request = effects
            .iter()
            .find_map(|e| match e {
                Effect::FetchImages(requests) => requests.first().cloned(),
                _ => None,
            })
            .expect("the page asks for its picture");
        let _ = window.run(effects);
        (window, request)
    }

    fn picture(window: &mut Window, request: &crate::app::ImageRequest, result: Picture) {
        let _ = window.update(Message::Outcome(Outcome::Picture {
            link: request.link,
            target: request.target.clone(),
            result,
        }));
    }

    /// The picture is fetched, decoded and drawn under its link at its
    /// natural size on the cell grid: 240 by 96 pixels are 30 by 6 cells.
    /// While it loads, the line under the link says so. The window with
    /// the picture is the golden.
    #[test]
    fn a_picture_is_drawn_under_its_link() {
        let (mut window, request) = picture_window();
        picture(&mut window, &request, Picture::Started);
        assert_eq!(window.model.images[0].state, SlotState::Loading);
        picture(&mut window, &request, Picture::Bytes(png_bytes(240, 96)));
        let SlotState::Ready { cells, .. } = window.model.images[0].state else {
            panic!("{:?}", window.model.images[0].state);
        };
        assert_eq!(cells, (30, 6));
        let (w, h) = (WINDOW.0 as u32, WINDOW.1 as u32);
        same_as("gui-picture-light.png", &shoot(&window), w, h);
    }

    /// A picture that is no picture is a line saying so, as in the
    /// terminal; a failed transfer the same.
    #[test]
    fn a_payload_that_does_not_decode_is_a_line() {
        let (mut window, request) = picture_window();
        picture(&mut window, &request, Picture::Bytes(b"not a png".to_vec()));
        assert!(
            matches!(window.model.images[0].state, SlotState::Text { .. }),
            "{:?}",
            window.model.images[0].state
        );
        picture(
            &mut window,
            &request,
            Picture::Failed("timeout".to_string()),
        );
        assert!(matches!(window.model.images[0].state, SlotState::Failed(_)));
    }

    /// A revisit draws the picture from the cache at once, with no request
    /// to the queue.
    #[test]
    fn a_revisit_draws_the_picture_from_the_cache() {
        let (mut window, request) = picture_window();
        picture(&mut window, &request, Picture::Bytes(png_bytes(240, 96)));
        window.pictures.clear();
        let effects = crate::app::event::plan_images(&mut window.model);
        assert_eq!(window.model.images[0].state, SlotState::Queued);
        let _ = window.run(effects);
        assert!(
            matches!(window.model.images[0].state, SlotState::Ready { .. }),
            "{:?}",
            window.model.images[0].state
        );
        assert!(window.pictures.holds(request.link));
    }

    /// A narrower window fits a wide picture to the page again: never
    /// wider than the page's columns, its height shrunk with it.
    #[test]
    fn a_resize_fits_the_picture_to_the_page_again() {
        let (mut window, request) = picture_window();
        picture(&mut window, &request, Picture::Bytes(png_bytes(1200, 120)));
        let SlotState::Ready { cells: wide, .. } = window.model.images[0].state else {
            panic!("ready");
        };
        let _ = window.update(Message::Resized(Size::new(500.0, 800.0)));
        let SlotState::Ready { cells: narrow, .. } = window.model.images[0].state else {
            panic!("ready");
        };
        assert!(narrow.0 < wide.0, "{narrow:?} {wide:?}");
        assert!(usize::from(narrow.0) <= window.model.width, "{narrow:?}");
        assert!(narrow.1 < wide.1, "{narrow:?} {wide:?}");
    }

    /// The pointer over a link run sets the model's hover, so the status
    /// line says where it goes, as in the terminal; off it, nothing.
    #[test]
    fn the_pointer_over_a_link_shows_where_it_goes() {
        let mut window = readme(Theme::Light);
        let (at, n) = first_link(&window);
        let page = iced::Point::new(at.x, at.y - BAR - ADDRESS_ROW);
        let _ = window.update(Message::Pointer(Some(page)));
        assert_eq!(window.model.hover, Some(n));
        assert!(window.model.scene().status.is_some(), "the status says");
        let _ = window.update(Message::Pointer(None));
        assert_eq!(window.model.hover, None);
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
