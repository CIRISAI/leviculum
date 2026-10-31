//! `loquax gui`: the window over the same shell and model as the terminal
//! (`docs/src/concepts/gui-toolkit.md`, option B, iced on the desktop;
//! `docs/src/concepts/loquax-map.md`, slice (d)). It opens on the chat
//! ([`chat`], 1145, after the phone's design): the conversations on the
//! left, the open one on the right. The map is its other screen: the peers
//! with a position listed on the left, by the display rule with their fix's
//! age and dimmed when stale, and the map on the right.
//!
//! Two halves on two threads:
//!
//! - The shell ([`crate::shell::Shell`]) runs on the tokio runtime, as in
//!   the terminal: engine, store, sends and fetches, the mailbox's schedule,
//!   the model and its clock. [`drive`] pumps it and ticks it, and after
//!   every change sends what the window shows ([`Seen`]).
//! - The window runs iced on the main thread. It holds only what is the
//!   window's: the map's view at the exact zoom a wheel or a touchpad
//!   leaves and the selected pin, the frame on screen, and the map's
//!   frames ([`loquax::mapframe`], the phone's pipeline, 1144), whose
//!   frames run on iced's executor so a drag never waits for one. A frame
//!   is composed from the tile cache with margins past the screen and
//!   shown where the view moved it until the next lands; while a drag or
//!   the wheel moves the view the names drawn at rest are kept, and at
//!   rest, with the window focused, the cache prefetches around the view.
//!
//! The map's autoload (1089) runs in the shell: the window decides what to
//! ask with the model's own rules ([`want_pins`] when the pins first arrive,
//! [`want_view`] on `d`) and sends it over; the shell's [`Seen`] carries the
//! loading word and the maps directory's epoch, and a new epoch reads the
//! regions again and paints the view anew.

pub mod chat;
pub mod map;

use std::process::ExitCode;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use iced::advanced::image::{FilterMethod, Handle};
use iced::widget::canvas::{self, Canvas, Frame, Geometry, Path};
use iced::widget::{button, column, container, row, scrollable, stack, text, text_editor};
use iced::{
    keyboard, mouse, window, Color, Element, Length, Point, Rectangle, Renderer, Size,
    Subscription, Task, Theme,
};
use tokio::sync::oneshot;

use leviculum_map::mercator::World;
use leviculum_map::viewport::{ExactView, MILLI};
use leviculum_mapfill::style::Style;
use loquax::mapframe::{hold, onto, Ask, Frames};
use loquax::picture::map_theme;
use loquax::tiles::Motion;
use loquax::ui::chat::{list_rows, ListRow, Open};
use loquax::ui::map::{fit, want_pins, want_view, zoom_floor, MapPin, MapSetup, MapWant};
use loquax::ui::{UiEvent, MAP_LOADED_ALREADY};
use loquax::when::WallTime;

use crate::shell::{self, local_now, Shell, ShellOptions, PUMP_MS, TICK_MS};

/// The list of peers on the left, in logical pixels.
const LIST_WIDTH: f32 = 260.0;
/// The window's size when it opens.
const WINDOW: (f32, f32) = (1100.0, 720.0);

/// What the window shows of the model, sent after every change.
#[derive(Clone, Debug, PartialEq)]
pub struct Seen {
    /// The map's pins in `Tab` order: every peer with a position, our own
    /// named `you`, each by the display rule with its fix's age.
    pub pins: Vec<MapPin>,
    /// The newest note the model holds, said in the bar.
    pub notice: Option<String>,
    /// The word while a map region loads.
    pub loading: Option<String>,
    /// The maps directory's epoch: a new one means a region landed or left.
    pub maps: u64,
    /// Whether going to a place loads its region (`map_autoload`).
    pub autoload: bool,
    /// How the map is drawn: the PC's finish in the table `map_theme`
    /// names, light for `auto`: the map keeps its own toggle (1133).
    pub style: Style,
    /// The conversation list, as the model orders it.
    pub chats: Vec<ListRow>,
    /// The open conversation.
    pub chat: Option<Open>,
    /// The model's wall clock, for the list's times and the date lines.
    pub now: Option<WallTime>,
}

impl Seen {
    fn of(shell: &Shell) -> Self {
        Self {
            pins: shell.model.map_pins(),
            notice: shell.model.toasts.last().map(|t| t.text.clone()),
            loading: shell.model.map_loading.clone(),
            maps: shell.maps.epoch(),
            autoload: shell.model.map_autoload,
            style: Style::PC.in_theme(map_theme(shell.model.map_theme, None)),
            chats: list_rows(&shell.model),
            chat: loquax::ui::chat::open(&shell.model),
            now: shell.model.now,
        }
    }
}

/// What the window asks of the shell: a map region to load, or an event
/// for the model, as a tap or a key on the phone hands it one.
#[derive(Clone, Debug)]
pub enum Want {
    Map(MapWant),
    Ui(UiEvent),
}

/// `loquax gui`: the shell started as the terminal starts it, driven on
/// the runtime while the window runs, and finished as the terminal
/// finishes it.
pub async fn run(options: ShellOptions) -> ExitCode {
    let (shell, _config) = match shell::start(&options).await {
        Ok(started) => started,
        Err(code) => return code,
    };
    let frames = Frames::new(shell.maps.clone());
    let (seen_tx, seen_rx) = unbounded();
    let (asks_tx, asks_rx) = tokio::sync::mpsc::unbounded_channel();
    let (leave_tx, leave_rx) = oneshot::channel();
    let driver = tokio::spawn(drive(shell, leave_rx, seen_tx, asks_rx));
    // iced runs its own loop on this thread until the window closes; the
    // runtime's other threads drive the shell meanwhile.
    let shown = tokio::task::block_in_place(|| window(frames, seen_rx, asks_tx));
    let _ = leave_tx.send(());
    let shell = match driver.await {
        Ok(shell) => shell,
        Err(error) => return crate::failure(error, "failed"),
    };
    shell::finish(shell, shown.map_err(|e| e.to_string())).await
}

/// The shell's loop while the window is open: the engine's queue every
/// [`PUMP_MS`], the clock every [`TICK_MS`], and what the window shows sent
/// whenever it changed. Ends when the window leaves or the model quits.
async fn drive(
    mut shell: Shell,
    mut leave: oneshot::Receiver<()>,
    seen: UnboundedSender<Seen>,
    mut asks: tokio::sync::mpsc::UnboundedReceiver<Want>,
) -> Shell {
    let mut tick = tokio::time::interval(Duration::from_millis(TICK_MS));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut pump = tokio::time::interval(Duration::from_millis(PUMP_MS));
    pump.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    shell.apply(UiEvent::Clock(local_now()));
    let mut clocks = (local_now().unix_secs, shell.elapsed_ms());
    let mut last: Option<Seen> = None;
    loop {
        // The map view asks for no copy, link or mouse capture; what the
        // model asks of a terminal has no meaning in the window yet.
        shell.outputs.clear();
        let now = Seen::of(&shell);
        if last.as_ref() != Some(&now) {
            if seen.unbounded_send(now.clone()).is_err() {
                break;
            }
            last = Some(now);
        }
        if shell.model.quit {
            break;
        }
        tokio::select! {
            _ = &mut leave => break,
            Some(want) = asks.recv() => match want {
                Want::Map(want) => shell.map_ask(want),
                Want::Ui(event) => shell.apply(event),
            },
            _ = pump.tick() => shell.poll(),
            _ = tick.tick() => shell.tick(&mut clocks),
        }
    }
    shell
}

/// Open the window and run it until it is closed.
fn window(frames: Frames, seen: UnboundedReceiver<Seen>, asks: Asks) -> iced::Result {
    // iced asks its boot function for the first state; it is asked once,
    // and what it needs is handed over through this slot.
    let slot = Mutex::new(Some((frames, seen, asks)));
    iced::application(
        move || {
            let taken = slot.lock().unwrap_or_else(PoisonError::into_inner).take();
            App::boot(taken)
        },
        App::update,
        App::view,
    )
    .title("Loquax")
    .subscription(App::subscription)
    .theme(App::theme)
    .window_size(Size::new(WINDOW.0, WINDOW.1))
    .run()
}

/// A pointer on the map, in logical pixels from its top left corner.
#[derive(Clone, Copy, Debug)]
pub enum Pointer {
    Down(Point),
    Move(Point),
    Up,
    /// The wheel or the touchpad turned at a point, by thousandths of a
    /// zoom step: towards the reader zooms in.
    Wheel(Point, i32),
}

#[derive(Clone, Debug)]
pub enum Message {
    Seen(Box<Seen>),
    /// The map's canvas has this logical size.
    MapSize(Size),
    Scale(f32),
    Window(window::Event),
    Key(keyboard::Event),
    Pointer(Pointer),
    /// A peer clicked in the list.
    Select([u8; 16]),
    Painted(Arc<Shown>),
    /// The wheel may have come to rest.
    WheelRest,
    /// A prefetch batch ended; whether more is to draw.
    Prefetched(bool),
    /// The desktop's light or dark setting, at the start and on a change.
    SystemTheme(iced::theme::Mode),
    /// The list's toggle: the other theme than the one shown.
    ToggleTheme,
    /// A conversation clicked in the list.
    Open([u8; 16]),
    Search(String),
    /// The list's new conversation: its field, what is typed, `Enter`.
    NewConversation,
    NewAddress(String),
    NewGo,
    Compose(text_editor::Action),
    /// `Enter` in the compose field, or the send button.
    Send,
    ToggleEmoji,
    /// An emoji picked into the compose field.
    Insert(String),
    /// The map, with this peer's pin selected; or back to the chat.
    ShowMap(Option<[u8; 16]>),
    ShowChats,
    /// A right click on the bubble with this key opens its reactions and
    /// actions; none closes them.
    Menu(Option<u64>),
    React([u8; 32], String),
    Reply([u8; 32]),
    CancelReply,
    Copy(String),
    /// The first click on delete asks, the second deletes.
    AskDelete(u64),
    Delete([u8; 16], u64),
}

impl Message {
    /// Whether it is the chat's, which [`App::chat`] folds.
    fn is_chat(&self) -> bool {
        !matches!(
            self,
            Message::Seen(_)
                | Message::MapSize(_)
                | Message::Scale(_)
                | Message::Window(_)
                | Message::Key(_)
                | Message::Pointer(_)
                | Message::Select(_)
                | Message::Painted(_)
                | Message::WheelRest
                | Message::Prefetched(_)
        )
    }
}

/// The window's two screens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Chats,
    Map,
}

/// Thousandths of a step a line of the wheel zooms: a notch is a step.
const WHEEL_LINE: f32 = 1000.0;
/// Pixels of a touchpad's scroll a zoom step takes.
const WHEEL_PX_PER_STEP: f32 = 120.0;
/// The wheel is at rest this long after its last turn: the frame at rest
/// places the names.
const WHEEL_REST_MS: u64 = 200;
/// The frame is drawn this part of the screen past each edge, so a drag
/// reveals drawn map while the next frame is drawn.
const MARGIN_PART: u32 = 8;

/// Where the window sends what it asks of the shell.
type Asks = tokio::sync::mpsc::UnboundedSender<Want>;

/// One frame the window asks for: everything the worker needs, and what
/// tells whether the frame on screen is still the one to show.
#[derive(Clone, Debug, PartialEq)]
pub struct Order {
    /// The screen's view, held.
    pub view: ExactView,
    pub margin: (u32, u32),
    pub moving: bool,
    pub style: Style,
    pub scale: f32,
    pub pins: Vec<MapPin>,
    pub selected: Option<[u8; 16]>,
    pub zooms: (u8, u8),
    /// The maps directory's epoch it is asked at.
    pub epoch: u64,
    pub motion: Motion,
}

/// A frame drawn for the window.
#[derive(Debug)]
pub struct Shown {
    pub order: Order,
    pub handle: Handle,
    /// The frame drawn, margins included.
    pub view: ExactView,
    /// The pins in its pixels.
    pub placed: Vec<map::Placed>,
    pub attribution: String,
    /// Tiles it owes: the window asks again.
    pub missing: usize,
}

/// What the worker does next.
#[derive(Debug)]
enum Job {
    Draw(Order),
    Prefetch(u64),
}

/// `order` drawn through the map's frames ([`loquax::mapframe`], the
/// phone's pipeline): the tiles composed at the exact zoom, while the
/// wheel turns at the tile step one to one for the canvas to scale
/// (1155), the names placed at rest and kept while moving, the pins
/// placed with the window's font and left for its canvas.
pub fn draw(frames: &Mutex<Frames>, order: Order) -> Arc<Shown> {
    let mut frames = frames.lock().unwrap_or_else(PoisonError::into_inner);
    frames.set_motion(order.motion);
    let shot = frames.frame(&Ask {
        view: order.view,
        margin: order.margin,
        moving: order.moving,
        style: order.style,
        scale: order.scale,
        pins: &order.pins,
        selected: order.selected,
        zooms: order.zooms,
        draw_pins: false,
        measure: Some(map::measure),
        scaled_while_moving: true,
    });
    let (w, h) = shot.view.size();
    Arc::new(Shown {
        handle: Handle::from_rgba(w, h, shot.rgba),
        view: shot.view,
        placed: frames.placed().to_vec(),
        attribution: shot.attribution,
        missing: shot.missing,
        order,
    })
}

/// One prefetch batch; whether more is to draw.
fn prefetch(frames: &Mutex<Frames>, now_ms: u64) -> bool {
    let mut frames = frames.lock().unwrap_or_else(PoisonError::into_inner);
    frames.prefetch(now_ms).more
}

/// A message after `ms`, from a thread that sleeps: the window's
/// executor has no timer.
fn after(ms: u64, message: Message) -> Task<Message> {
    let (tx, rx) = futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(ms));
        let _ = tx.send(());
    });
    Task::perform(
        async move {
            let _ = rx.await;
        },
        move |()| message.clone(),
    )
}

/// A press on the map, in screen pixels.
#[derive(Clone, Copy, Debug)]
struct Press {
    from: (i32, i32),
    last: (i32, i32),
    pin: Option<[u8; 16]>,
    dragging: bool,
}

/// The window's state.
struct App {
    seen: Seen,
    asks: Option<Asks>,
    /// The view the last fit chose: a region landing under it fits again.
    fitted_at: Option<(World, u32)>,
    frames: Arc<Mutex<Frames>>,
    setup: MapSetup,
    /// The map's view in screen pixels, at the zoom the wheel left.
    view: ExactView,
    selected: Option<[u8; 16]>,
    /// The map's canvas in logical pixels, once laid out.
    size: Option<Size>,
    /// Screen pixels per logical pixel.
    scale: f32,
    /// The view fitted to the pins once they first arrived.
    fitted: bool,
    press: Option<Press>,
    /// The frame on screen.
    shown: Option<Arc<Shown>>,
    /// A frame or a prefetch batch runs on the worker; what is asked
    /// meanwhile is asked when it ends.
    busy: bool,
    /// The wheel's last turn, while it may still turn.
    wheel: Option<Instant>,
    /// A wake for the wheel's rest is set.
    wheel_timer: bool,
    /// The window has the focus: only then does it prefetch.
    focused: bool,
    /// The prefetch around the frame on screen may have more to draw.
    prefetching: bool,
    motion: Motion,
    origin: Instant,
    screen: Screen,
    /// The window's size in logical pixels.
    window: Size,
    /// The desktop's setting, dark when it says so, and whether the
    /// toggle turned it round.
    system_dark: bool,
    flipped: bool,
    search: String,
    /// The new conversation's field while it is open.
    new_address: Option<String>,
    compose: text_editor::Content,
    /// The message a reply being written answers, with its first line.
    reply: Option<([u8; 32], String)>,
    emoji: bool,
    /// The bubble whose reactions and actions are open, and one whose
    /// delete was asked once.
    menu: Option<u64>,
    deleting: Option<u64>,
}

impl App {
    fn boot(taken: Option<(Frames, UnboundedReceiver<Seen>, Asks)>) -> (Self, Task<Message>) {
        let (frames, seen, asks) = match taken {
            Some((frames, seen, asks)) => (frames, Some(seen), Some(asks)),
            None => (Frames::new(loquax::picture::Maps::default()), None, None),
        };
        let app = Self::new(frames, asks);
        let scale = window::oldest()
            .and_then(window::scale_factor)
            .map(Message::Scale);
        let seen = match seen {
            Some(seen) => Task::run(seen, |seen| Message::Seen(Box::new(seen))),
            None => Task::none(),
        };
        let theme = iced::system::theme().map(Message::SystemTheme);
        (app, Task::batch([scale, seen, theme]))
    }

    fn new(frames: Frames, asks: Option<Asks>) -> Self {
        let setup = map::setup(frames.painter());
        let centre = fit(&[], map::area((1, 1)), &setup).0;
        Self {
            seen: Seen {
                pins: Vec::new(),
                notice: None,
                loading: None,
                maps: 0,
                autoload: false,
                style: Style::PC,
                chats: Vec::new(),
                chat: None,
                now: None,
            },
            asks,
            fitted_at: None,
            frames: Arc::new(Mutex::new(frames)),
            view: ExactView::new(centre, u32::from(setup.zooms.0) * MILLI, 1, 1),
            setup,
            selected: None,
            size: None,
            scale: 1.0,
            fitted: false,
            press: None,
            shown: None,
            busy: false,
            wheel: None,
            wheel_timer: false,
            focused: true,
            prefetching: false,
            motion: Motion::default(),
            origin: Instant::now(),
            screen: Screen::Chats,
            window: Size::new(WINDOW.0, WINDOW.1),
            system_dark: false,
            flipped: false,
            search: String::new(),
            new_address: None,
            compose: text_editor::Content::new(),
            reply: None,
            emoji: false,
            menu: None,
            deleting: None,
        }
    }

    /// Whether the window is dark: the desktop's setting, turned round by
    /// the toggle.
    fn dark(&self) -> bool {
        self.system_dark != self.flipped
    }

    fn theme(&self) -> Theme {
        match self.dark() {
            true => Theme::Dark,
            false => Theme::Light,
        }
    }

    /// Hand `event` to the model.
    fn tell(&self, event: UiEvent) {
        if let Some(asks) = &self.asks {
            let _ = asks.send(Want::Ui(event));
        }
    }

    /// What a chat message changes; a task for what iced does for it.
    fn chat(&mut self, message: Message) -> Task<Message> {
        let open = self.seen.chat.as_ref().map(|c| c.peer);
        match message {
            Message::SystemTheme(mode) => self.system_dark = mode == iced::theme::Mode::Dark,
            Message::ToggleTheme => self.flipped = !self.flipped,
            Message::Open(peer) => {
                self.menu = None;
                self.reply = None;
                self.tell(UiEvent::Open { peer });
            }
            Message::Search(query) => self.search = query,
            Message::NewConversation => {
                self.new_address = match self.new_address {
                    Some(_) => None,
                    None => Some(String::new()),
                }
            }
            Message::NewAddress(typed) => self.new_address = Some(typed),
            Message::NewGo => {
                let typed = self.new_address.clone().unwrap_or_default();
                match loquax::address::parse(typed.trim()) {
                    Ok(peer) => {
                        self.new_address = None;
                        self.tell(UiEvent::Open { peer });
                    }
                    Err(error) => self.seen.notice = Some(format!("'{}': {error}", typed.trim())),
                }
            }
            Message::Compose(action) => self.compose.perform(action),
            Message::Send => {
                let body = self.compose.text();
                let body = body.trim_end_matches('\n').to_string();
                let Some(peer) = open else {
                    return Task::none();
                };
                if body.trim().is_empty() {
                    return Task::none();
                }
                self.compose = text_editor::Content::new();
                self.emoji = false;
                match self.reply.take() {
                    Some((target, _)) => self.tell(UiEvent::Reply { peer, target, body }),
                    None => self.tell(UiEvent::Write { peer, body }),
                }
            }
            Message::ToggleEmoji => self.emoji = !self.emoji,
            Message::Insert(emoji) => {
                self.compose
                    .perform(text_editor::Action::Edit(text_editor::Edit::Paste(
                        Arc::new(emoji),
                    )))
            }
            Message::ShowMap(peer) => {
                self.screen = Screen::Map;
                if let Some(peer) = peer {
                    self.apply(Message::Select(peer));
                }
                return self.paint();
            }
            Message::ShowChats => self.screen = Screen::Chats,
            Message::Menu(key) => {
                self.menu = key;
                self.deleting = None;
            }
            Message::React(target, content) => {
                self.menu = None;
                if let Some(peer) = open {
                    self.tell(UiEvent::React {
                        peer,
                        target,
                        content,
                    });
                }
            }
            Message::Reply(target) => {
                self.menu = None;
                let quoted = self
                    .seen
                    .chat
                    .as_ref()
                    .and_then(|c| c.bubbles.iter().find(|b| b.id == Some(target)))
                    .map(|b| b.body.lines().next().unwrap_or_default().to_string())
                    .unwrap_or_default();
                self.reply = Some((target, quoted));
            }
            Message::CancelReply => self.reply = None,
            Message::Copy(words) => {
                self.menu = None;
                return iced::clipboard::write(words);
            }
            Message::AskDelete(key) => self.deleting = Some(key),
            Message::Delete(peer, key) => {
                self.menu = None;
                self.deleting = None;
                self.tell(UiEvent::Delete {
                    peer,
                    keys: vec![key],
                });
            }
            _ => {}
        }
        Task::none()
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            window::events().map(|(_, event)| Message::Window(event)),
            keyboard::listen().map(Message::Key),
            iced::system::theme_changes().map(Message::SystemTheme),
        ])
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// The map's size in screen pixels.
    fn pixels(&self) -> Option<(u32, u32)> {
        let size = self.size?;
        let px = |v: f32| (v * self.scale).round().max(1.0) as u32;
        Some((px(size.width), px(size.height)))
    }

    /// The zooms the view keeps to: the screen's floor, the regions'
    /// deepest.
    fn zooms(&self) -> (u8, u8) {
        let size = self.pixels().unwrap_or((1, 1));
        (zoom_floor(map::area(size), &self.setup), self.setup.zooms.1)
    }

    /// The view at the canvas's size, held to one world and the zooms.
    fn hold(&mut self) {
        if let Some((w, h)) = self.pixels() {
            self.view.resize(w, h);
        }
        let (floor, ceiling) = self.zooms();
        self.view = hold(self.view, floor, ceiling);
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        if message.is_chat() {
            return self.chat(message);
        }
        if !self.apply(message) {
            return Task::none();
        }
        self.paint()
    }

    /// What `message` changes; whether the map may need a frame.
    fn apply(&mut self, message: Message) -> bool {
        match message {
            Message::Seen(seen) => {
                let landed = seen.maps != self.seen.maps;
                self.seen = *seen;
                if landed {
                    self.regions_changed();
                }
                if !self.fitted {
                    self.fit();
                }
            }
            Message::MapSize(size) => {
                self.size = Some(size);
                self.hold();
                if !self.fitted {
                    self.fit();
                }
            }
            Message::Scale(scale) | Message::Window(window::Event::Rescaled(scale)) => {
                self.scale = scale.max(0.25);
                self.hold();
            }
            Message::Window(window::Event::Resized(size)) => {
                self.window = size;
                return false;
            }
            Message::Window(window::Event::Focused) => self.focused = true,
            Message::Window(window::Event::Unfocused) => self.focused = false,
            Message::Window(_) => return false,
            Message::Key(event) => self.key(event),
            Message::Pointer(pointer) => self.pointer(pointer),
            Message::Select(peer) => {
                self.selected = Some(peer);
                if let Some(pin) = self.seen.pins.iter().find(|p| p.peer == peer) {
                    self.view.centre = pin.at;
                    self.hold();
                }
            }
            Message::Painted(shown) => {
                self.busy = false;
                self.shown = Some(shown);
                self.prefetching = true;
            }
            Message::WheelRest => {
                self.wheel_timer = false;
                let rested = self
                    .wheel
                    .is_some_and(|at| at.elapsed() >= Duration::from_millis(WHEEL_REST_MS));
                if rested {
                    self.wheel = None;
                }
            }
            Message::Prefetched(more) => {
                self.busy = false;
                self.prefetching = more;
            }
            _ => return false,
        }
        true
    }

    /// The worker's next job, when it is free: the frame the view asks
    /// for unless it is on screen and owes nothing, else a prefetch batch
    /// while the frame on screen rests and the window has the focus.
    fn next_job(&mut self) -> Option<Job> {
        if self.busy || self.screen != Screen::Map {
            return None;
        }
        // The first job since the last pan freezes the heading the
        // prefetch plans by, before the order carries it (1189).
        self.motion.asked(self.now_ms());
        let order = self.order()?;
        let current = self
            .shown
            .as_ref()
            .is_some_and(|s| s.order == order && s.missing == 0);
        let job = match current {
            false => Job::Draw(order),
            true if self.prefetching && self.focused && !order.moving => {
                Job::Prefetch(self.now_ms())
            }
            true => return None,
        };
        self.busy = true;
        Some(job)
    }

    /// The frame the view asks for now.
    fn order(&self) -> Option<Order> {
        let (w, h) = self.pixels()?;
        Some(Order {
            view: self.view,
            margin: (w / MARGIN_PART, h / MARGIN_PART),
            moving: self.moving(),
            style: self.seen.style,
            scale: self.scale,
            pins: self.seen.pins.clone(),
            selected: self.selected,
            zooms: self.zooms(),
            epoch: self.seen.maps,
            motion: self.motion,
        })
    }

    /// A drag or the wheel moves the view.
    fn moving(&self) -> bool {
        self.press.is_some_and(|p| p.dragging) || self.wheel.is_some()
    }

    /// Run the worker's next job on iced's executor, so a drag never
    /// waits for a frame; and wake for the wheel's rest.
    fn paint(&mut self) -> Task<Message> {
        let mut tasks = Vec::new();
        if self.wheel.is_some() && !self.wheel_timer {
            self.wheel_timer = true;
            tasks.push(after(WHEEL_REST_MS, Message::WheelRest));
        }
        let frames = self.frames.clone();
        match self.next_job() {
            Some(Job::Draw(order)) => tasks.push(Task::perform(
                async move { draw(&frames, order) },
                Message::Painted,
            )),
            Some(Job::Prefetch(now)) => tasks.push(Task::perform(
                async move { prefetch(&frames, now) },
                Message::Prefetched,
            )),
            None => {}
        }
        Task::batch(tasks)
    }

    /// Every pin in view, as `0` does; before the first pins and the
    /// canvas, nothing. The first fit with pins asks for the places no
    /// region holds (1089).
    fn fit(&mut self) {
        let Some(size) = self.pixels() else {
            return;
        };
        let (centre, zoom) = fit(&self.seen.pins, map::area(size), &self.setup);
        self.view = ExactView::new(centre, u32::from(zoom) * MILLI, size.0, size.1);
        self.hold();
        self.fitted_at = Some((self.view.centre, self.view.zoom_milli()));
        if self.selected.is_none() {
            self.selected = self.seen.pins.first().map(|p| p.peer);
        }
        let first = !self.fitted && !self.seen.pins.is_empty();
        self.fitted = !self.seen.pins.is_empty();
        if first && self.seen.autoload {
            for want in want_pins(&self.seen.pins, &self.setup.regions) {
                self.ask(want);
            }
        }
    }

    fn ask(&self, want: MapWant) {
        if let Some(asks) = &self.asks {
            let _ = asks.send(Want::Map(want));
        }
    }

    /// A region landed or left: the regions read again, the setup with
    /// them, and the fit again when the reader has not moved since it.
    /// The tiles drop themselves on the new epoch.
    fn regions_changed(&mut self) {
        {
            let mut frames = self.frames.lock().unwrap_or_else(PoisonError::into_inner);
            frames.refresh();
            self.setup = map::setup(frames.painter());
        }
        if self.fitted_at == Some((self.view.centre, self.view.zoom_milli())) {
            let selected = self.selected;
            self.fit();
            self.selected = selected.or(self.selected);
        }
    }

    /// The view zoomed to `zoom_milli`, within the zooms, about the
    /// screen pixel `(x, y)`.
    fn zoom_to(&mut self, zoom_milli: i64, x: f64, y: f64) {
        let (floor, ceiling) = self.zooms();
        let (floor, ceiling) = (
            i64::from(floor) * 1000,
            i64::from(ceiling.max(floor)) * 1000,
        );
        let z = zoom_milli.clamp(floor, ceiling);
        self.motion.zoom(z - i64::from(self.view.zoom_milli()));
        self.view
            .zoom_about(u32::try_from(z).unwrap_or(MILLI), x, y);
        self.hold();
    }

    /// The map's keys, on the map alone: on the chat a key is typing.
    fn key(&mut self, event: keyboard::Event) {
        if self.screen != Screen::Map {
            return;
        }
        let keyboard::Event::KeyPressed { modified_key, .. } = event else {
            return;
        };
        if modified_key == keyboard::Key::Named(keyboard::key::Named::Escape) {
            self.screen = Screen::Chats;
            return;
        }
        let keyboard::Key::Character(c) = modified_key.as_ref() else {
            return;
        };
        let (w, h) = self.view.size();
        let (cx, cy) = (f64::from(w) / 2.0, f64::from(h) / 2.0);
        let z = i64::from(self.view.zoom_milli());
        match c {
            "+" | "=" => self.zoom_to(z + 1000, cx, cy),
            "-" => self.zoom_to(z - 1000, cx, cy),
            "0" => {
                let selected = self.selected;
                self.fit();
                self.selected = selected.or(self.selected);
            }
            // The visible area's region, as `d` on the terminal's map.
            "d" => {
                let view = self.view.at_zoom(self.view.tile_zoom());
                match want_view(&view, &self.setup.regions) {
                    Some(want) => self.ask(want),
                    None => self.seen.notice = Some(MAP_LOADED_ALREADY.to_string()),
                }
            }
            _ => {}
        }
    }

    /// Where the frame on screen lands on the view now: its top left
    /// corner and its scale ([`loquax::mapframe::onto`]).
    fn onto(&self) -> Option<(&Shown, f64, f64, f64)> {
        let shown = self.shown.as_deref()?;
        let (x, y, k) = onto(&shown.view, &self.view);
        Some((shown, x, y, k))
    }

    /// The pin under the screen pixel `(x, y)`, of the frame on screen.
    fn hit(&self, x: i32, y: i32) -> Option<[u8; 16]> {
        let (shown, tx, ty, k) = self.onto()?;
        let px = (f64::from(x) - tx) / k;
        let py = (f64::from(y) - ty) / k;
        map::hit(&shown.placed, px as f32, py as f32, self.scale)
    }

    fn pointer(&mut self, pointer: Pointer) {
        if self.pixels().is_none() {
            return;
        }
        let at = |p: Point| {
            (
                (p.x * self.scale).round() as i32,
                (p.y * self.scale).round() as i32,
            )
        };
        match pointer {
            Pointer::Down(p) => {
                let (x, y) = at(p);
                self.press = Some(Press {
                    from: (x, y),
                    last: (x, y),
                    pin: self.hit(x, y),
                    dragging: false,
                });
            }
            Pointer::Move(p) => {
                let (x, y) = at(p);
                let Some(mut press) = self.press else {
                    return;
                };
                let far = (map::DRAG * self.scale) as i32;
                press.dragging |=
                    (x - press.from.0).abs() >= far || (y - press.from.1).abs() >= far;
                if press.dragging {
                    let (dx, dy) = (press.last.0 - x, press.last.1 - y);
                    self.view.pan(dx, dy);
                    self.motion.pan(f64::from(dx), f64::from(dy), self.now_ms());
                    press.last = (x, y);
                    self.hold();
                }
                self.press = Some(press);
            }
            Pointer::Up => {
                if let Some(press) = self.press.take() {
                    if !press.dragging {
                        if let Some(peer) = press.pin {
                            self.selected = Some(peer);
                        }
                    }
                }
            }
            Pointer::Wheel(p, by) => {
                let (x, y) = at(p);
                self.wheel = Some(Instant::now());
                let z = i64::from(self.view.zoom_milli()) + i64::from(by);
                self.zoom_to(z, f64::from(x), f64::from(y));
            }
        }
    }

    fn view(&self) -> Element<'_, Message> {
        match self.screen {
            Screen::Chats => row![self.list_pane(), self.chat_pane()].into(),
            Screen::Map => self.map_view(),
        }
    }

    fn map_view(&self) -> Element<'_, Message> {
        let list = self
            .seen
            .pins
            .iter()
            .fold(column![].spacing(2), |list, pin| {
                let alpha = if pin.stale { 0.5 } else { 1.0 };
                let age = pin.age.clone().unwrap_or_default();
                let entry = column![text(pin.label.clone()).size(15), text(age).size(12),];
                let style = match Some(pin.peer) == self.selected {
                    true => button::primary,
                    false => button::text,
                };
                let button = button(container(entry).width(Length::Fill))
                    .on_press(Message::Select(pin.peer))
                    .style(move |theme: &Theme, status| {
                        let mut s = style(theme, status);
                        s.text_color = s.text_color.scale_alpha(alpha);
                        s
                    })
                    .width(Length::Fill);
                list.push(button)
            });
        let list: Element<'_, Message> = match self.seen.pins.is_empty() {
            true => text("no positions yet").size(14).into(),
            false => scrollable(list).into(),
        };
        let credit = self
            .shown
            .as_ref()
            .map_or(String::new(), |s| s.attribution.clone());
        let bar = row![
            button(text("Chats").size(13))
                .on_press(Message::ShowChats)
                .padding([0, 6])
                .style(button::text),
            // Always on screen: the region's credit, or the outline's.
            text(credit).size(13),
            container(
                text(
                    [self.seen.loading.clone(), self.seen.notice.clone()]
                        .into_iter()
                        .flatten()
                        .collect::<Vec<_>>()
                        .join(" · ")
                )
                .size(13)
            )
            .width(Length::Fill)
            .align_right(Length::Fill),
            text(format!("  z{:.1}", self.view.zoom())).size(13),
        ]
        .padding([2, 8]);
        // The picture is a canvas of its own under the pins' canvas: iced
        // draws a canvas's images above its fills, which would hide the
        // markers. On a canvas it is drawn where the view moved it since
        // it was asked for, scaled when the wheel zoomed meanwhile.
        let picture = Canvas::new(PictureCanvas { app: self })
            .width(Length::Fill)
            .height(Length::Fill);
        let canvas = Canvas::new(MapCanvas { app: self })
            .width(Length::Fill)
            .height(Length::Fill);
        let map = container(
            stack![picture, canvas]
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .clip(true);
        row![
            container(list).width(LIST_WIDTH).padding(6),
            column![map, bar],
        ]
        .into()
    }
}

/// The frame on screen, where the view moved it.
struct PictureCanvas<'a> {
    app: &'a App,
}

impl canvas::Program<Message> for PictureCanvas<'_> {
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
        if let Some((shown, x, y, k)) = self.app.onto() {
            let scale = f64::from(self.app.scale);
            let (w, h) = shown.view.size();
            let filter = match (k - 1.0).abs() < 1e-9 {
                true => FilterMethod::Nearest,
                false => FilterMethod::Linear,
            };
            let at = Rectangle {
                x: (x / scale) as f32,
                y: (y / scale) as f32,
                width: (f64::from(w) * k / scale) as f32,
                height: (f64::from(h) * k / scale) as f32,
            };
            frame.draw_image(
                at,
                canvas::Image::new(shown.handle.clone())
                    .filter_method(filter)
                    .snap(true),
            );
        }
        vec![frame.into_geometry()]
    }
}

/// The map's canvas over the picture: the pins, their labels, and the
/// pointer.
struct MapCanvas<'a> {
    app: &'a App,
}

/// What the canvas remembers between events.
#[derive(Default)]
struct CanvasState {
    size: Option<Size>,
    pressed: bool,
}

impl canvas::Program<Message> for MapCanvas<'_> {
    type State = CanvasState;

    fn update(
        &self,
        state: &mut CanvasState,
        event: &iced::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        if state.size != Some(bounds.size()) {
            state.size = Some(bounds.size());
            return Some(canvas::Action::publish(Message::MapSize(bounds.size())));
        }
        let iced::Event::Mouse(event) = event else {
            return None;
        };
        let inside = cursor.position_in(bounds);
        let pointer = match (event, inside) {
            (mouse::Event::ButtonPressed(mouse::Button::Left), Some(p)) => {
                state.pressed = true;
                Pointer::Down(p)
            }
            (mouse::Event::CursorMoved { .. }, _) if state.pressed => {
                let p = cursor.position()?;
                Pointer::Move(Point::new(p.x - bounds.x, p.y - bounds.y))
            }
            (mouse::Event::ButtonReleased(mouse::Button::Left), _) if state.pressed => {
                state.pressed = false;
                Pointer::Up
            }
            (mouse::Event::WheelScrolled { delta }, Some(p)) => {
                let by = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => y * WHEEL_LINE,
                    mouse::ScrollDelta::Pixels { y, .. } => y * 1000.0 / WHEEL_PX_PER_STEP,
                };
                let by = by.round() as i32;
                if by == 0 {
                    return None;
                }
                Pointer::Wheel(p, by)
            }
            _ => return None,
        };
        Some(canvas::Action::publish(Message::Pointer(pointer)).and_capture())
    }

    fn draw(
        &self,
        _state: &CanvasState,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let app = self.app;
        let scale = app.scale;
        let Some((shown, tx, ty, k)) = app.onto() else {
            return vec![frame.into_geometry()];
        };
        // The pins as the frame on screen placed them, moved with it.
        let to = |x: f32, y: f32| {
            let s = f64::from(scale);
            Point::new(
                ((tx + k * f64::from(x)) / s) as f32,
                ((ty + k * f64::from(y)) / s) as f32,
            )
        };
        for pin in &shown.placed {
            let ink = match (pin.selected, pin.stale) {
                (true, _) => Color::from_rgb8(0xd0, 0x30, 0x30),
                (false, false) => Color::from_rgb8(0x20, 0x50, 0xc0),
                (false, true) => Color::from_rgba8(0x20, 0x50, 0xc0, 0.45),
            };
            let centre = to(pin.x, pin.y);
            let radius = map::MARKER / 2.0;
            frame.fill(&Path::circle(centre, radius + 1.5), Color::WHITE);
            frame.fill(&Path::circle(centre, radius), ink);
            if let Some((x, y, words)) = &pin.label {
                let text_ink = match pin.stale {
                    true => Color::from_rgba8(0x20, 0x20, 0x20, 0.55),
                    false => Color::from_rgb8(0x10, 0x10, 0x10),
                };
                let at = to(*x, *y);
                let width = pin.width / scale;
                let line = map::LABEL * map::LINE;
                // A light ground under the text, so a name reads on any
                // street.
                frame.fill_rectangle(
                    Point::new(at.x - 2.0, at.y),
                    Size::new(width + 4.0, line),
                    Color::from_rgba8(0xff, 0xff, 0xff, 0.75),
                );
                frame.fill_text(canvas::Text {
                    content: words.clone(),
                    position: at,
                    color: text_ink,
                    size: iced::Pixels(map::LABEL),
                    ..canvas::Text::default()
                });
            }
        }
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &CanvasState,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        match (state.pressed, cursor.is_over(bounds)) {
            (true, _) => mouse::Interaction::Grabbing,
            (false, true) => mouse::Interaction::Grab,
            (false, false) => mouse::Interaction::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_map::mercator::{project, LatLon};
    use map::tests::{florence, pin};

    /// The window over the Florence fixture, 480 x 320 at a density of
    /// one, `pins` seen, the view at `zoom_milli` over Piazza d'Azeglio.
    fn window_over_florence(pins: Vec<MapPin>, zoom_milli: u32) -> App {
        let frames = florence();
        let maps = frames.painter().epoch();
        let mut app = App::new(frames, None);
        app.screen = Screen::Map;
        app.apply(Message::MapSize(Size::new(480.0, 320.0)));
        app.apply(Message::Seen(Box::new(Seen {
            pins,
            notice: None,
            loading: None,
            maps,
            autoload: false,
            style: Style::PC,
            chats: Vec::new(),
            chat: None,
            now: None,
        })));
        let at = project(LatLon::from_degrees(43.7752, 11.2660));
        app.view = ExactView::new(at, zoom_milli, 480, 320);
        app.hold();
        app
    }

    /// The worker's jobs run here until it has none, as iced runs them:
    /// the last frame drawn, and whether a prefetch was offered after it
    /// (not run).
    fn run(app: &mut App) -> (Option<Arc<Shown>>, bool) {
        let mut last = None;
        loop {
            match app.next_job() {
                Some(Job::Draw(order)) => {
                    let shown = draw(&app.frames, order);
                    app.apply(Message::Painted(shown.clone()));
                    last = Some(shown);
                }
                Some(Job::Prefetch(_)) => {
                    app.busy = false;
                    return (last, true);
                }
                None => return (last, false),
            }
        }
    }

    fn rgba(shown: &Shown) -> Vec<u8> {
        match &shown.handle {
            Handle::Rgba { pixels, .. } => pixels.to_vec(),
            other => panic!("not raw pixels: {other:?}"),
        }
    }

    fn names(app: &App) -> Vec<String> {
        app.frames
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .names()
    }

    /// A drag keeps the names (1144): the frame at rest places them, a
    /// drag of 40 pixels to the left asks for a moving frame, which draws
    /// the same names where the drag moved them, so where the two frames
    /// overlap they are the same pixels, 40 apart. The frame on screen
    /// moves with the pointer until the next lands, and the pins with it.
    /// The control: the same moving frame from frames that never rested
    /// has no names, and differs there.
    #[test]
    fn a_drag_keeps_the_names() {
        let pins = vec![pin(1, "alice", 43.7752, 11.2640)];
        let mut app = window_over_florence(pins, 16 * MILLI);
        let (rest, _) = run(&mut app);
        let rest = rest.expect("a frame at rest");
        assert!(!rest.order.moving);
        let named = names(&app);
        assert!(named.iter().any(|n| n == "Piazza D'Azeglio"), "{named:?}");
        // The frame sits a margin past the screen's corner, within the
        // pixel its centre was moved to.
        let (x0, y0, _) = onto(&rest.view, &app.view);
        let (mx, my) = rest.order.margin;
        assert!((x0 + f64::from(mx)).abs() < 1.0 && (y0 + f64::from(my)).abs() < 1.0);
        app.apply(Message::Pointer(Pointer::Down(Point::new(200.0, 150.0))));
        app.apply(Message::Pointer(Pointer::Move(Point::new(160.0, 150.0))));
        // Before the next frame lands, the one on screen moved with it.
        let (x, y, k) = onto(&rest.view, &app.view);
        assert!((k - 1.0).abs() < 1e-9);
        assert!((x - (x0 - 40.0)).abs() < 1e-6, "{x0} {x}");
        assert!((y - y0).abs() < 1e-6, "{y0} {y}");
        let (moved, _) = run(&mut app);
        let moved = moved.expect("a moving frame");
        assert!(moved.order.moving);
        assert_eq!(names(&app), named, "the names are kept");
        let (w, h) = rest.view.size();
        assert_eq!(moved.view.size(), (w, h));
        let (a, b) = (rgba(&rest), rgba(&moved));
        let px = |v: &[u8], x: u32, y: u32| {
            let i = ((y * w + x) * 4) as usize;
            [v[i], v[i + 1], v[i + 2], v[i + 3]]
        };
        let overlap = |v: &[u8]| {
            (0..h)
                .flat_map(|y| (0..w - 40).map(move |x| (x, y)))
                .filter(|&(x, y)| px(v, x, y) != px(&a, x + 40, y))
                .count()
        };
        assert_eq!(overlap(&b), 0, "the moved frame is the rest frame, moved");
        let (p0, p1) = (&rest.placed[0], &moved.placed[0]);
        assert!((p1.x - (p0.x - 40.0)).abs() < 1e-3, "{p0:?} {p1:?}");
        let control = draw(&Mutex::new(florence()), moved.order.clone());
        let differ = overlap(&rgba(&control));
        assert!(differ > 1000, "without kept names: {differ} pixels differ");
    }

    /// A fractional wheel zoom stays where it ends (1144): a touchpad's
    /// 0.37 of a step about a point keeps the world point there and the
    /// zoom between the steps; the frames while the wheel turns are moving
    /// ones, and once it rests the frame at rest is drawn at that very
    /// zoom, with its names, and nothing asks for another: the next job is
    /// the prefetch.
    #[test]
    fn a_fractional_wheel_zoom_stays_where_it_ends() {
        let mut app = window_over_florence(Vec::new(), 14 * MILLI);
        run(&mut app);
        let under = (100.0, 80.0);
        let held = app.view.to_world(under.0, under.1);
        app.apply(Message::Pointer(Pointer::Wheel(
            Point::new(100.0, 80.0),
            370,
        )));
        assert_eq!(app.view.zoom_milli(), 14_370);
        let (x, y) = app.view.to_screen(held);
        assert!(
            (x - under.0).abs() < 1.0 && (y - under.1).abs() < 1.0,
            "{x} {y}"
        );
        let (turning, _) = run(&mut app);
        let turning = turning.expect("a frame while the wheel turns");
        assert!(turning.order.moving);
        // While it turns the frame is the step one to one (1155), and
        // the image scales it to the view: the point stays under it.
        assert_eq!(turning.view.zoom_milli(), 14_000);
        let (tx, ty, k) = onto(&turning.view, &app.view);
        assert!((k - 2f64.powf(0.37)).abs() < 1e-9, "{k}");
        let (fx, fy) = turning.view.to_screen(held);
        let (x, y) = (tx + k * fx, ty + k * fy);
        assert!(
            (x - under.0).abs() < 1.0 && (y - under.1).abs() < 1.0,
            "{x} {y}"
        );
        std::thread::sleep(Duration::from_millis(WHEEL_REST_MS + 50));
        app.apply(Message::WheelRest);
        let (rested, prefetch) = run(&mut app);
        let rested = rested.expect("the frame at rest");
        assert!(!rested.order.moving);
        assert_eq!(app.view.zoom_milli(), 14_370);
        assert_eq!(rested.view.zoom_milli(), 14_370);
        assert!(prefetch, "at rest the next job is the prefetch");
        assert!(!names(&app).is_empty());
        let (x, y) = app.view.to_screen(held);
        assert!(
            (x - under.0).abs() < 1.0 && (y - under.1).abs() < 1.0,
            "{x} {y}"
        );
    }

    use loquax::outbox::OutboxEvent;
    use loquax::ui::update;

    const ANNA: [u8; 16] = [0x3a; 16];
    const BERT: [u8; 16] = [0xb7; 16];

    fn at(day: u32, hour: u32, minute: u32) -> WallTime {
        WallTime::from_civil(2026, 10, day, hour, minute, 3600)
    }

    fn received(model: &mut crate::shell::Model, from: [u8; 16], id: u8, body: &str) {
        update(
            model,
            UiEvent::Engine(OutboxEvent::Received {
                message_id: [id; 32],
                source: from,
                title: Vec::new(),
                body: body.as_bytes().to_vec(),
                fields: Vec::new(),
                verified: true,
            }),
        );
    }

    /// A model as the shell holds one: Anna wrote yesterday and twice this
    /// morning, we answered, she reacted; Bert wrote once, unread. Anna's
    /// conversation is open.
    fn chat_model() -> crate::shell::Model {
        let mut model = crate::shell::Model::new(&[]);
        model.names.remember(ANNA, "Anna Schulz");
        model.names.remember(BERT, "Bert");
        update(&mut model, UiEvent::Clock(at(29, 18, 40)));
        received(
            &mut model,
            ANNA,
            1,
            "Are you coming to the market tomorrow?",
        );
        update(&mut model, UiEvent::Clock(at(30, 9, 12)));
        received(&mut model, ANNA, 2, "Good morning!");
        update(&mut model, UiEvent::Clock(at(30, 9, 13)));
        received(
            &mut model,
            ANNA,
            3,
            "We meet at the fountain at ten, the stall with the apples.",
        );
        update(&mut model, UiEvent::Open { peer: ANNA });
        update(&mut model, UiEvent::Clock(at(30, 9, 20)));
        update(
            &mut model,
            UiEvent::Write {
                peer: ANNA,
                body: "Yes, see you there".to_string(),
            },
        );
        update(
            &mut model,
            UiEvent::React {
                peer: ANNA,
                target: [3; 32],
                content: "ok".to_string(),
            },
        );
        update(&mut model, UiEvent::Clock(at(30, 9, 40)));
        received(&mut model, BERT, 4, "The relay on the hill is up again");
        update(&mut model, UiEvent::Open { peer: ANNA });
        model
    }

    fn seen_of(model: &crate::shell::Model) -> Seen {
        Seen {
            pins: model.map_pins(),
            notice: None,
            loading: None,
            maps: 0,
            autoload: false,
            style: Style::PC,
            chats: list_rows(model),
            chat: loquax::ui::chat::open(model),
            now: model.now,
        }
    }

    /// The window over `chat_model`, with the channel to the shell.
    fn chat_window() -> (App, tokio::sync::mpsc::UnboundedReceiver<Want>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(Frames::new(loquax::picture::Maps::default()), Some(tx));
        app.apply(Message::Seen(Box::new(seen_of(&chat_model()))));
        (app, rx)
    }

    fn told(rx: &mut tokio::sync::mpsc::UnboundedReceiver<Want>) -> Vec<UiEvent> {
        let mut events = Vec::new();
        while let Ok(want) = rx.try_recv() {
            if let Want::Ui(event) = want {
                events.push(event);
            }
        }
        events
    }

    fn type_into(app: &mut App, words: &str) {
        for c in words.chars() {
            let _ = app.update(Message::Compose(text_editor::Action::Edit(
                text_editor::Edit::Insert(c),
            )));
        }
    }

    /// The list and the open chat are the model's, through the library's
    /// rows ([`loquax::ui::chat`]): the newest conversation first with its
    /// unread count, the open one's bubbles grouped as the phone groups
    /// them, our reaction a chip on the bubble it names.
    #[test]
    fn the_window_shows_the_models_list_and_chat() {
        let (app, _rx) = chat_window();
        let names: Vec<&str> = app.seen.chats.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(names[0].starts_with("Bert"), "{names:?}");
        assert_eq!(app.seen.chats[0].unread, 1);
        let open = app.seen.chat.as_ref().expect("Anna's chat is open");
        assert_eq!(open.peer, ANNA);
        assert_eq!(open.initials, "AS");
        let bodies: Vec<&str> = open.bubbles.iter().map(|b| b.body.as_str()).collect();
        assert_eq!(bodies.len(), 4, "the reaction is no bubble: {bodies:?}");
        let days: Vec<bool> = open.placements.iter().map(|p| p.day).collect();
        assert_eq!(days, [true, true, false, false]);
        assert!(open.placements[2].joins_before);
        assert_eq!(open.bubbles[2].chips.len(), 1);
        assert!(open.bubbles[2].chips[0].ours);
        assert!(open.bubbles[3].ours);
        assert_eq!(open.bubbles[3].mark, Some(loquax::ui::chat::Mark::Queued));
    }

    /// `Enter` sends what was typed to the open conversation and empties
    /// the field; a reply asked for on a bubble goes as a reply; blank
    /// text sends nothing.
    #[test]
    fn enter_sends_and_a_reply_goes_as_one() {
        let (mut app, mut rx) = chat_window();
        let _ = app.update(Message::Send);
        assert!(told(&mut rx).is_empty(), "nothing typed, nothing sent");
        type_into(&mut app, "on my way");
        let _ = app.update(Message::Send);
        assert_eq!(
            told(&mut rx),
            [UiEvent::Write {
                peer: ANNA,
                body: "on my way".to_string()
            }]
        );
        assert_eq!(app.compose.text().trim(), "");
        let _ = app.update(Message::Menu(Some(2)));
        let _ = app.update(Message::Reply([2; 32]));
        assert_eq!(app.menu, None);
        assert_eq!(app.reply, Some(([2; 32], "Good morning!".to_string())));
        type_into(&mut app, "morning");
        let _ = app.update(Message::Send);
        assert_eq!(
            told(&mut rx),
            [UiEvent::Reply {
                peer: ANNA,
                target: [2; 32],
                body: "morning".to_string()
            }]
        );
        assert_eq!(app.reply, None);
    }

    /// A click on a row opens its conversation in the model; a reaction
    /// picked on a bubble reacts; a delete asks once, then deletes.
    #[test]
    fn the_list_opens_and_the_actions_tell_the_model() {
        let (mut app, mut rx) = chat_window();
        let _ = app.update(Message::Open(BERT));
        let _ = app.update(Message::React([1; 32], "👍".to_string()));
        assert_eq!(
            told(&mut rx),
            [
                UiEvent::Open { peer: BERT },
                UiEvent::React {
                    peer: ANNA,
                    target: [1; 32],
                    content: "👍".to_string()
                }
            ]
        );
        let _ = app.update(Message::AskDelete(7));
        assert_eq!(app.deleting, Some(7));
        assert!(told(&mut rx).is_empty(), "the first click only asks");
        let _ = app.update(Message::Delete(ANNA, 7));
        assert_eq!(
            told(&mut rx),
            [UiEvent::Delete {
                peer: ANNA,
                keys: vec![7]
            }]
        );
    }

    /// The new conversation's field opens what an address names and says
    /// what is wrong with anything else.
    #[test]
    fn a_new_conversation_takes_an_address() {
        let (mut app, mut rx) = chat_window();
        let _ = app.update(Message::NewConversation);
        let _ = app.update(Message::NewAddress("not hex".to_string()));
        let _ = app.update(Message::NewGo);
        assert!(told(&mut rx).is_empty());
        assert!(app.seen.notice.is_some());
        assert!(app.new_address.is_some(), "the field stays for a fix");
        let hex = loquax::address::to_hex(&BERT);
        let _ = app.update(Message::NewAddress(hex));
        let _ = app.update(Message::NewGo);
        assert_eq!(told(&mut rx), [UiEvent::Open { peer: BERT }]);
        assert_eq!(app.new_address, None);
    }

    /// On the chat a key is typing: `+` does not zoom the map behind it,
    /// and the map draws nothing until it is shown. On the map it zooms,
    /// and `Escape` goes back.
    #[test]
    fn the_maps_keys_belong_to_the_map() {
        let (mut app, _rx) = chat_window();
        app.apply(Message::MapSize(Size::new(480.0, 320.0)));
        let zoom = app.view.zoom_milli();
        let plus = keyboard::Event::KeyPressed {
            key: keyboard::Key::Character("+".into()),
            modified_key: keyboard::Key::Character("+".into()),
            physical_key: keyboard::key::Physical::Unidentified(
                keyboard::key::NativeCode::Unidentified,
            ),
            location: keyboard::Location::Standard,
            modifiers: keyboard::Modifiers::empty(),
            text: None,
            repeat: false,
        };
        app.apply(Message::Key(plus.clone()));
        assert_eq!(app.view.zoom_milli(), zoom);
        assert!(app.next_job().is_none(), "no frame for a hidden map");
        let _ = app.update(Message::ShowMap(None));
        app.busy = false;
        app.apply(Message::Key(plus));
        assert!(app.view.zoom_milli() > zoom);
        let escape = keyboard::Event::KeyPressed {
            key: keyboard::Key::Named(keyboard::key::Named::Escape),
            modified_key: keyboard::Key::Named(keyboard::key::Named::Escape),
            physical_key: keyboard::key::Physical::Unidentified(
                keyboard::key::NativeCode::Unidentified,
            ),
            location: keyboard::Location::Standard,
            modifiers: keyboard::Modifiers::empty(),
            text: None,
            repeat: false,
        };
        app.apply(Message::Key(escape));
        assert_eq!(app.screen, Screen::Chats);
    }

    /// The window's view drawn off screen by iced's own software renderer,
    /// `w` x `h` at a density of one, as RGB.
    fn shoot(app: &App, w: u32, h: u32) -> Vec<u8> {
        use iced::advanced::renderer::{self, Headless, Renderer as _};
        use iced::advanced::widget::Tree;
        use iced::advanced::{layout, Layout};
        let mut renderer = futures::executor::block_on(<Renderer as Headless>::new(
            iced::Font::DEFAULT,
            iced::Pixels(16.0),
            None,
        ))
        .expect("the software renderer");
        let size = Size::new(w as f32, h as f32);
        let mut element = app.view();
        let mut tree = Tree::new(&element);
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, size),
        );
        let viewport = Rectangle::with_size(size);
        renderer.reset(viewport);
        let theme = app.theme();
        element.as_widget().draw(
            &tree,
            &mut renderer,
            &theme,
            &renderer::Style {
                text_color: chat::colour(app.tokens().text),
            },
            Layout::new(&node),
            mouse::Cursor::Unavailable,
            &viewport,
        );
        let background = chat::colour(app.tokens().background);
        renderer
            .screenshot(iced::Size::new(w, h), 1.0, background)
            .chunks(4)
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect()
    }

    /// The chat pane in each theme (1145), pixel for pixel the goldens:
    /// the list with Bert's unread count and Anna's row open, Anna's chat
    /// with yesterday's date line, her two grouped bubbles, our reaction's
    /// pill, our bubble right with its queued mark, the compose bar. The
    /// two themes differ, and a second picture is the first.
    #[test]
    fn the_chat_pane_is_the_golden_in_both_themes() {
        let (w, h) = (900, 560);
        let (mut app, _rx) = chat_window();
        app.window = Size::new(w as f32, h as f32);
        let light = shoot(&app, w, h);
        assert_eq!(shoot(&app, w, h), light, "a second picture is the same");
        let _ = app.update(Message::ToggleTheme);
        assert!(app.dark());
        let dark = shoot(&app, w, h);
        assert_ne!(light, dark);
        // The ground is the token's in each.
        assert_eq!(&light[..3], &[0xff, 0xff, 0xff]);
        assert_eq!(&dark[..3], &[0x17, 0x18, 0x1b]);
        map::tests::same_as("gui-chat-light.png", &light, w, h);
        map::tests::same_as("gui-chat-dark.png", &dark, w, h);
    }

    /// The desktop's dark setting turns the window dark, and the toggle
    /// turns it round either way.
    #[test]
    fn the_theme_follows_the_desktop_and_the_toggle() {
        let (mut app, _rx) = chat_window();
        assert!(!app.dark());
        let _ = app.update(Message::SystemTheme(iced::theme::Mode::Dark));
        assert!(app.dark());
        let _ = app.update(Message::ToggleTheme);
        assert!(!app.dark());
        let _ = app.update(Message::SystemTheme(iced::theme::Mode::Light));
        assert!(app.dark());
    }
}
