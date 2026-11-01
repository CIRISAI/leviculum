//! `loquax gui`: the window over the same shell and model as the terminal
//! (`docs/src/concepts/gui-toolkit.md`, option B, iced on the desktop;
//! `docs/src/concepts/loquax-map.md`, slice (d)). It opens on the chat
//! ([`chat`], 1145, after the phone's design), drawn from the model's
//! scene (1218, [`loquax::ui::scene`]): the tabs, the conversations and
//! the open one beside them in a wide window, stacked as on the phone in
//! a narrow one. A press on a node and a key of the keymap are intents
//! ([`loquax::ui::intent::Intent`]), the model's sent to the shell, the
//! window's kept. The map is its other screen: the peers
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
pub mod viewer;

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
use leviculum_ui::input::{KeyCode, KeyEvent, KeyModifiers};
use leviculum_ui::keymap::Ctx;
use loquax::keys::{self, MapCmd};
use loquax::mapframe::{hold, onto, Ask, Frames};
use loquax::picture::map_theme;
use loquax::tiles::Motion;
use loquax::ui::intent::{Intent, Messages, Target};
use loquax::ui::map::{fit, want_pins, want_view, zoom_floor, MapPin, MapSetup, MapWant};
use loquax::ui::scene::{LayoutClass, NodeId, Scene, TabId};
use loquax::ui::strings::Lang;
use loquax::ui::{Pane, UiEvent, MAP_LOADED_ALREADY};
use loquax::when::WallTime;

use crate::shell::{self, local_now, Shell, ShellOptions, PUMP_MS, TICK_MS};

/// The list of peers on the left, in logical pixels.
const LIST_WIDTH: f32 = 260.0;
/// The window's size when it opens.
const WINDOW: (f32, f32) = (1100.0, 720.0);
/// From this width on the window is Wide (1218): the rail, the list and
/// a chat of 300 pixels beside each other; below it stacks.
const WIDE_FROM: f32 = chat::RAIL + chat::LIST_WIDTH + 300.0;

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
    /// The list and the open chat (1218): the scene at
    /// [`LayoutClass::Wide`], which the window draws in the class its
    /// width gives ([`Scene::in_class`]).
    pub scene: Scene,
    /// The model's wall clock, for the list's times and the date lines.
    pub now: Option<WallTime>,
    /// The reader's switch "Send read receipts" (1233), the settings'
    /// row.
    pub receipts: bool,
    /// The photo a press on a bubble's photo last asked to see whole
    /// (1269), decoded by the shell: a new one opens the viewer.
    pub photo: Option<viewer::Picture>,
}

impl Seen {
    fn of(shell: &Shell) -> Self {
        Self {
            pins: shell.model.map_pins(),
            notice: shell.model.toasts.last().map(|t| t.text.clone()),
            loading: shell.model.map_loading.clone(),
            maps: shell.maps.epoch(),
            autoload: shell.model.map_autoload,
            style: Style::SCREEN.in_theme(map_theme(shell.model.map_theme, None)),
            scene: shell.model.scene(LayoutClass::Wide),
            now: shell.model.now,
            receipts: shell.model.reading.on,
            photo: None,
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
    let mut photo: Option<viewer::Picture> = None;
    loop {
        // The map view asks for no copy, link or mouse capture; what the
        // model asks of a terminal has no meaning in the window yet, but a
        // photo, which the window's viewer shows (1269), and a file, which
        // the system's handler opens (1221).
        for output in shell.outputs.drain(..) {
            let path = match output {
                shell::Output::Photo(path) => match picture(&path, photo.as_ref()) {
                    Ok(decoded) => {
                        photo = Some(decoded);
                        continue;
                    }
                    // What the window cannot decode the system may show.
                    Err(error) => {
                        tracing::warn!("{}: {error}", path.display());
                        path
                    }
                },
                shell::Output::File(path) => path,
                _ => continue,
            };
            if let Err(error) = open::that(&path) {
                tracing::warn!("could not open {}: {error}", path.display());
            }
        }
        let mut now = Seen::of(&shell);
        now.photo = photo.clone();
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

/// The photo file at `path` decoded for the viewer, its serial past
/// `last`'s.
fn picture(
    path: &std::path::Path,
    last: Option<&viewer::Picture>,
) -> Result<viewer::Picture, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let pixels = loquax::photo::fit(&bytes, 4096).map_err(|e| e.to_string())?;
    Ok(viewer::Picture {
        serial: last.map_or(1, |p| p.serial + 1),
        width: pixels.width,
        height: pixels.height,
        full: pixels.full,
        rgba: Arc::new(pixels.rgba),
    })
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
    /// What the list's search field holds.
    Search(String),
    /// The new conversation's field: what is typed, `Enter`.
    NewAddress(String),
    NewGo,
    Compose(text_editor::Action),
    /// What the user asks (1218): a press on a scene node raises the
    /// node's [`Intent`], a key the keymap's, as on every target.
    Intent(Intent),
    /// The top bar's gear: the settings page (1233).
    Settings,
    /// The mouse on the photo viewer (1269).
    Viewer(viewer::Input),
    /// The viewer's Close.
    ViewerClose,
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
                | Message::Intent(_)
                | Message::Pointer(_)
                | Message::Select(_)
                | Message::Painted(_)
                | Message::WheelRest
                | Message::Prefetched(_)
                | Message::Settings
                | Message::Viewer(_)
                | Message::ViewerClose
        )
    }
}

/// The window's two screens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Chats,
    Map,
    /// The settings (1233): one page, "Send read receipts" on it.
    Settings,
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
    /// A name fades in or out: the window asks again.
    pub fading: bool,
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
    let now_ms = frames.clock_ms();
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
        now_ms: Some(now_ms),
        scaled_while_moving: true,
    });
    let (w, h) = shot.view.size();
    Arc::new(Shown {
        handle: Handle::from_rgba(w, h, shot.rgba),
        view: shot.view,
        placed: frames.placed().to_vec(),
        attribution: shot.attribution,
        missing: shot.missing,
        fading: shot.fading,
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
    /// The chat whose draft the compose field holds (1219): the model
    /// keeps it, the field is put back from the scene when another opens.
    compose_for: Option<[u8; 16]>,
    emoji: bool,
    /// The bubble whose reactions and actions are open, and one whose
    /// delete was asked once.
    menu: Option<NodeId>,
    deleting: Option<NodeId>,
    /// The language the scene's words are said in: the desktop's locale.
    lang: Lang,
    /// The scene of [`Seen`] in the window's class: what is drawn.
    scene: Scene,
    /// The photo whole while it is open (1269), over everything.
    viewer: Option<viewer::Viewer>,
}

impl App {
    fn boot(taken: Option<(Frames, UnboundedReceiver<Seen>, Asks)>) -> (Self, Task<Message>) {
        let (frames, seen, asks) = match taken {
            Some((frames, seen, asks)) => (frames, Some(seen), Some(asks)),
            None => (Frames::new(loquax::picture::Maps::default()), None, None),
        };
        let mut app = Self::new(frames, asks);
        app.lang = locale();
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
        let empty = crate::shell::Model::new(&[]).scene(LayoutClass::Wide);
        Self {
            seen: Seen {
                pins: Vec::new(),
                notice: None,
                loading: None,
                maps: 0,
                autoload: false,
                style: Style::SCREEN,
                scene: empty.clone(),
                now: None,
                receipts: true,
                photo: None,
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
            compose_for: None,
            emoji: false,
            menu: None,
            deleting: None,
            lang: Lang::En,
            scene: empty,
            viewer: None,
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
        match message {
            Message::SystemTheme(mode) => self.system_dark = mode == iced::theme::Mode::Dark,
            Message::Search(query) => self.search = query,
            Message::NewAddress(typed) => self.new_address = Some(typed),
            Message::NewGo => {
                let typed = self.new_address.clone().unwrap_or_default();
                // The one reader every frontend asks (1263): a contact
                // code, an address, and what is neither in plain words.
                match loquax::code::read(&typed).map(|code| code.address()) {
                    Ok(Some(peer)) => {
                        self.new_address = None;
                        return self.intent(Intent::OpenChat { peer });
                    }
                    Ok(None) => {
                        self.seen.notice = Some(
                            loquax::ui::strings::StrId::CodeIsMailbox
                                .text(self.lang)
                                .to_string(),
                        )
                    }
                    Err(why) => self.seen.notice = Some(why.say(self.lang).to_string()),
                }
            }
            Message::Compose(action) => {
                let edit = action.is_edit();
                self.compose.perform(action);
                if edit {
                    self.tell_draft();
                }
            }
            _ => {}
        }
        Task::none()
    }

    /// The model is told what the compose field holds, as its draft
    /// (1219), on every change of the text or the reply.
    fn tell_draft(&self) {
        let Some(peer) = self.compose_for else {
            return;
        };
        let typed = self.compose.text();
        self.tell(UiEvent::Draft {
            peer,
            text: typed.trim_end_matches('\n').to_string(),
            reply: self.reply.as_ref().map(|(id, _)| *id),
        });
    }

    /// The open chat's draft into the compose field, when another chat
    /// opened than the one the field holds (1219).
    fn restore_draft(&mut self) {
        let open = self.seen.scene.chat.as_ref().map(|c| c.peer);
        if open == self.compose_for {
            return;
        }
        self.compose_for = open;
        let Some(chat) = self.seen.scene.chat.as_ref() else {
            self.compose = text_editor::Content::new();
            self.reply = None;
            return;
        };
        self.compose = text_editor::Content::with_text(&chat.compose.text);
        self.reply = chat.compose.reply.map(|target| {
            let quoted = chat
                .groups
                .iter()
                .flat_map(|g| &g.bubbles)
                .find(|node| node.bubble.id == Some(target))
                .map(|node| {
                    node.bubble
                        .body
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .to_string()
                })
                .unwrap_or_default();
            (target, quoted)
        });
    }

    /// The peer an intent's target names: the open chat's for
    /// [`Target::Open`].
    fn target(&self, to: Target) -> Option<[u8; 16]> {
        match to {
            Target::Peer(peer) => Some(peer),
            Target::Open => self.scene.chat.as_ref().map(|c| c.peer),
        }
    }

    /// What `intent` does in the window (1218): a press on a scene node
    /// or a key of the keymap. What is the model's goes to the model as
    /// its event; what is the window's (the map's view, the menu, the
    /// reply being written, the emoji row, the theme) stays here. An
    /// intent the window has no part for does nothing.
    fn intent(&mut self, intent: Intent) -> Task<Message> {
        match intent {
            Intent::Activate(id) => {
                let action = self
                    .scene
                    .action(id)
                    .filter(|a| a.enabled)
                    .map(|a| a.intent.clone());
                if let Some(intent) = action {
                    return self.intent(intent);
                }
            }
            // The reply being written stays with its chat's draft: the
            // field takes the next chat's once the scene shows it (1219).
            Intent::OpenChat { peer } => {
                self.menu = None;
                self.tell(UiEvent::Open { peer });
            }
            Intent::CloseChat => {
                self.menu = None;
                self.tell(UiEvent::Close);
            }
            Intent::Back | Intent::Dismiss => match (self.menu, self.screen) {
                (Some(_), _) => {
                    self.menu = None;
                    self.deleting = None;
                }
                (None, Screen::Map | Screen::Settings) => self.screen = Screen::Chats,
                // A stacked chat goes back to the list, as its bar's back
                // does; beside the list Back has nothing to leave.
                (None, Screen::Chats) => {
                    if let Some(back) = self.scene.action(NodeId::TopBar) {
                        let back = back.intent.clone();
                        return self.intent(back);
                    }
                }
            },
            Intent::Theme => self.flipped = !self.flipped,
            Intent::ReadReceipts { on } => {
                self.tell(UiEvent::ReadReceipts(on.apply(self.seen.receipts)));
            }
            Intent::Search => {
                self.screen = Screen::Chats;
                return iced::widget::operation::focus(chat::SEARCH_FIELD);
            }
            Intent::NewConversation | Intent::AddContact { .. } => {
                self.new_address = match self.new_address {
                    Some(_) => None,
                    None => Some(String::new()),
                }
            }
            Intent::Send { to, body } => {
                let Some(peer) = self.target(to) else {
                    return Task::none();
                };
                let body = match body {
                    Some(body) => body,
                    None => {
                        let typed = self.compose.text();
                        typed.trim_end_matches('\n').to_string()
                    }
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
            Intent::Emoji => self.emoji = !self.emoji,
            Intent::Insert(emoji) => {
                self.compose
                    .perform(text_editor::Action::Edit(text_editor::Edit::Paste(
                        Arc::new(emoji),
                    )));
                self.tell_draft();
            }
            Intent::ShowMap { peer } => {
                self.screen = Screen::Map;
                if let Some(peer) = peer {
                    self.apply(Message::Select(peer));
                }
                return self.paint();
            }
            // A row's pin or the chat bar's map (1224): the map on the
            // peer's pin, selected, its card open.
            Intent::OpenMapAt { peer } => {
                let peer = self.target(peer);
                return self.intent(Intent::ShowMap { peer });
            }
            Intent::Tab(TabId::Map) => return self.intent(Intent::ShowMap { peer: None }),
            Intent::Tab(TabId::Chats) => self.screen = Screen::Chats,
            Intent::Menu(id) => {
                self.menu = id;
                self.deleting = None;
            }
            Intent::React {
                to,
                target: Some(target),
                content: Some(content),
            } => {
                self.menu = None;
                if let Some(peer) = self.target(to) {
                    self.tell(UiEvent::React {
                        peer,
                        target,
                        content,
                    });
                }
            }
            Intent::Reply {
                to,
                target,
                body: Some(body),
            } => {
                if let Some(peer) = self.target(to) {
                    self.tell(UiEvent::Reply { peer, target, body });
                }
            }
            Intent::Reply {
                target, body: None, ..
            } => {
                self.menu = None;
                let quoted = self
                    .scene
                    .chat
                    .iter()
                    .flat_map(|c| &c.groups)
                    .flat_map(|g| &g.bubbles)
                    .find(|node| node.bubble.id == Some(target))
                    .map(|node| {
                        node.bubble
                            .body
                            .lines()
                            .next()
                            .unwrap_or_default()
                            .to_string()
                    })
                    .unwrap_or_default();
                self.reply = Some((target, quoted));
                self.tell_draft();
            }
            Intent::CancelReply => {
                self.reply = None;
                self.tell_draft();
            }
            Intent::Copy => {
                let words = self
                    .menu
                    .and_then(|id| self.scene.bubble(id))
                    .map(|node| node.bubble.body.clone());
                self.menu = None;
                if let Some(words) = words {
                    return iced::clipboard::write(words);
                }
            }
            Intent::SendAgain { peer, key } => self.tell(UiEvent::SendAgain { peer, key }),
            Intent::ViewPhoto { peer, key } => self.tell(UiEvent::ViewPhoto { peer, key }),
            Intent::OpenFile { peer, key, index } => {
                self.tell(UiEvent::OpenFile { peer, key, index })
            }
            Intent::PlayVoice { peer, key } => self.tell(UiEvent::PlayVoice { peer, key }),
            Intent::PauseVoice { peer, key } => self.tell(UiEvent::PauseVoice { peer, key }),
            Intent::Delete {
                messages: Messages::Keys { peer, keys },
                confirmed,
            } => match confirmed {
                false => self.deleting = keys.first().map(|&key| NodeId::Bubble(key)),
                true => {
                    self.menu = None;
                    self.deleting = None;
                    self.tell(UiEvent::Delete { peer, keys });
                }
            },
            Intent::Map(cmd) if self.screen == Screen::Map => {
                self.map_cmd(cmd);
                return self.paint();
            }
            Intent::LoadMapArea if self.screen == Screen::Map => {
                let view = self.view.at_zoom(self.view.tile_zoom());
                match want_view(&view, &self.setup.regions) {
                    Some(want) => self.ask(want),
                    None => self.seen.notice = Some(MAP_LOADED_ALREADY.to_string()),
                }
            }
            _ => {}
        }
        Task::none()
    }

    /// The layout class the window's width gives (1218): the list beside
    /// the chat from [`WIDE_FROM`] logical pixels, stacked below.
    fn class(&self) -> LayoutClass {
        match self.window.width >= WIDE_FROM {
            true => LayoutClass::Wide,
            false => LayoutClass::Compact,
        }
    }

    /// The scene in the window's class, again after a change of either.
    fn rescene(&mut self) {
        self.scene = self.seen.scene.clone().in_class(self.class());
    }

    /// What a map key does on the map: zoom about the centre, pan by
    /// eighths of the view, fit every pin, step through the pins, open the
    /// pin's conversation, or close the map.
    fn map_cmd(&mut self, cmd: MapCmd) {
        let (w, h) = self.view.size();
        let (cx, cy) = (f64::from(w) / 2.0, f64::from(h) / 2.0);
        let z = i64::from(self.view.zoom_milli());
        match cmd {
            MapCmd::ZoomIn => self.zoom_to(z + 1000, cx, cy),
            MapCmd::ZoomOut => self.zoom_to(z - 1000, cx, cy),
            MapCmd::Pan(dx, dy) => {
                let eighth =
                    |side: u32, by: i8| i32::from(by) * i32::try_from(side / 8).unwrap_or(0);
                self.view.pan(eighth(w, dx), eighth(h, dy));
                self.hold();
            }
            MapCmd::Fit => {
                let selected = self.selected;
                self.fit();
                self.selected = selected.or(self.selected);
            }
            MapCmd::Step(next) => {
                let pins = &self.seen.pins;
                let at = pins.iter().position(|p| Some(p.peer) == self.selected);
                let n = pins.len();
                let step = match (at, next) {
                    (_, _) if n == 0 => None,
                    (None, _) => Some(0),
                    (Some(i), true) => Some((i + 1) % n),
                    (Some(i), false) => Some((i + n - 1) % n),
                };
                if let Some(peer) = step.map(|i| pins[i].peer) {
                    self.apply(Message::Select(peer));
                }
            }
            MapCmd::Open => {
                if let Some(peer) = self.selected {
                    self.screen = Screen::Chats;
                    self.tell(UiEvent::Open { peer });
                }
            }
            MapCmd::Close => self.screen = Screen::Chats,
            MapCmd::Drawing(_) | MapCmd::GlyphTest => {}
        }
    }

    /// The intent the keymap gives a key the widgets left alone (1218):
    /// the same chords as the terminal's ([`keys::resolve`]), on the map
    /// the map's, on a stacked chat the messages' (`Escape` is Back), on
    /// the chats the list's.
    fn key(&self, event: &keyboard::Event) -> Option<Intent> {
        let key = chord(event)?;
        let pane = match (self.screen, self.scene.top.id) {
            (Screen::Map, _) => Pane::Map,
            (Screen::Chats, NodeId::ChatBar) => Pane::Messages,
            (Screen::Chats | Screen::Settings, _) => Pane::Conversations,
        };
        keys::resolve(&key, keys::panes_of(pane), Ctx::ENHANCED).map(Intent::from_action)
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
        // The viewer is over everything: a key is its Esc or nothing.
        if let (Some(viewer), Message::Key(event)) = (&mut self.viewer, &message) {
            if chord(event).is_some_and(|k| k.code == KeyCode::Esc) && viewer.back() {
                self.viewer = None;
            }
            return Task::none();
        }
        match message {
            Message::Viewer(input) => {
                if let Some(viewer) = &mut self.viewer {
                    viewer.input(input, Instant::now());
                }
                return Task::none();
            }
            Message::ViewerClose => {
                self.viewer = None;
                return Task::none();
            }
            Message::Intent(intent) => return self.intent(intent),
            Message::Settings => {
                self.screen = Screen::Settings;
                self.menu = None;
                return Task::none();
            }
            Message::Key(event) => {
                return match self.key(&event) {
                    Some(intent) => self.intent(intent),
                    None => Task::none(),
                }
            }
            _ => {}
        }
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
                if let Some(photo) = seen
                    .photo
                    .as_ref()
                    .filter(|p| self.seen.photo.as_ref() != Some(p))
                {
                    self.viewer = Some(viewer::Viewer::new(photo, self.window));
                }
                self.seen = *seen;
                self.rescene();
                self.restore_draft();
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
                self.rescene();
                return false;
            }
            Message::Window(window::Event::Focused) => self.focused = true,
            Message::Window(window::Event::Unfocused) => self.focused = false,
            Message::Window(_) => return false,
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
            .is_some_and(|s| s.order == order && s.missing == 0 && !s.fading);
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
        if let Some(viewer) = &self.viewer {
            return viewer.view();
        }
        let body = match (self.screen, self.class(), &self.scene.chat) {
            (Screen::Map, _, _) => self.map_view(),
            (Screen::Settings, _, _) => self.settings_pane(),
            (Screen::Chats, LayoutClass::Wide, _) => {
                row![self.list_pane(), self.chat_pane()].into()
            }
            (Screen::Chats, LayoutClass::Compact, Some(_)) => self.chat_pane(),
            (Screen::Chats, LayoutClass::Compact, None) => self.list_pane(),
        };
        match (self.tabs(), self.class()) {
            (Some(tabs), LayoutClass::Wide) => row![tabs, body].into(),
            (Some(tabs), LayoutClass::Compact) => column![body, tabs].into(),
            (None, _) => body,
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
                .on_press(Message::Intent(Intent::Tab(TabId::Chats)))
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

/// The language the desktop's locale names (`LC_ALL`, `LC_MESSAGES`,
/// `LANG`, the first one set): the scene's words are said in it.
fn locale() -> Lang {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|value| !value.is_empty())
        .map_or(Lang::En, |tag| Lang::of_tag(&tag))
}

/// A key iced reports as the keymap's key (1218): the named keys the
/// chords use, a character as itself, the modifiers held. None for a
/// release or a key no chord names.
fn chord(event: &keyboard::Event) -> Option<KeyEvent> {
    use keyboard::key::Named;
    let keyboard::Event::KeyPressed {
        modified_key,
        modifiers,
        ..
    } = event
    else {
        return None;
    };
    let shift = modifiers.shift();
    let code = match modified_key.as_ref() {
        keyboard::Key::Named(named) => match named {
            Named::Escape => KeyCode::Esc,
            Named::Enter => KeyCode::Enter,
            Named::Tab if shift => KeyCode::BackTab,
            Named::Tab => KeyCode::Tab,
            Named::Backspace => KeyCode::Backspace,
            Named::Delete => KeyCode::Delete,
            Named::Insert => KeyCode::Insert,
            Named::ArrowLeft => KeyCode::Left,
            Named::ArrowRight => KeyCode::Right,
            Named::ArrowUp => KeyCode::Up,
            Named::ArrowDown => KeyCode::Down,
            Named::Home => KeyCode::Home,
            Named::End => KeyCode::End,
            Named::PageUp => KeyCode::PageUp,
            Named::PageDown => KeyCode::PageDown,
            Named::Space => KeyCode::Char(' '),
            Named::F1 => KeyCode::F(1),
            Named::F2 => KeyCode::F(2),
            Named::F3 => KeyCode::F(3),
            Named::F4 => KeyCode::F(4),
            Named::F5 => KeyCode::F(5),
            Named::F6 => KeyCode::F(6),
            Named::F7 => KeyCode::F(7),
            Named::F8 => KeyCode::F(8),
            Named::F9 => KeyCode::F(9),
            Named::F10 => KeyCode::F(10),
            Named::F11 => KeyCode::F(11),
            Named::F12 => KeyCode::F(12),
            _ => return None,
        },
        keyboard::Key::Character(c) => {
            let mut chars = c.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => KeyCode::Char(c),
                _ => return None,
            }
        }
        keyboard::Key::Unidentified => return None,
    };
    let mut held = KeyModifiers::NONE;
    if shift {
        held |= KeyModifiers::SHIFT;
    }
    if modifiers.control() {
        held |= KeyModifiers::CONTROL;
    }
    if modifiers.alt() {
        held |= KeyModifiers::ALT;
    }
    Some(KeyEvent::new(code, held))
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
            style: Style::SCREEN,
            scene: crate::shell::Model::new(&[]).scene(LayoutClass::Wide),
            now: None,
            receipts: true,
            photo: None,
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
    /// The same moving frame from frames that never rested places the
    /// same names itself (1225). The control: frames that set no names
    /// draw it without them, and differ there.
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
        let fresh = Mutex::new(florence());
        draw(&fresh, moved.order.clone());
        let placed = fresh.lock().unwrap_or_else(PoisonError::into_inner).names();
        assert_eq!(placed, named, "a moving frame places its own names");
        let mut ground = florence();
        ground.set_names_off(true);
        let control = draw(&Mutex::new(ground), moved.order.clone());
        let differ = overlap(&rgba(&control));
        assert!(differ > 1000, "without names: {differ} pixels differ");
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
    /// The reader.
    const OWN: [u8; 16] = [0x30; 16];

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

    /// A model as the shell holds one, the reader Mia: Anna wrote yesterday and twice this
    /// morning, we answered, she reacted; Bert wrote once, unread. Anna's
    /// conversation is open.
    fn chat_model() -> crate::shell::Model {
        let mut model = crate::shell::Model::new(&[]);
        model.address = Some(OWN);
        model.names.remember(OWN, "Mia Sander");
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
            style: Style::SCREEN,
            scene: model.scene(LayoutClass::Wide),
            now: model.now,
            receipts: model.reading.on,
            photo: None,
        }
    }

    /// A press on a bubble's photo (1269): the shell's file decoded for
    /// the window, its own size kept; the window opens the viewer over
    /// everything, the wheel zooms it, Esc goes to 1x and then closes, and
    /// the same photo seen again does not open it again, a new press does.
    #[test]
    fn a_photo_opens_in_the_windows_viewer_and_zooms() {
        let dir = std::env::temp_dir().join(format!("loquax-1269-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let path = dir.join("photo.png");
        image::RgbaImage::from_pixel(5000, 100, image::Rgba([200, 30, 30, 255]))
            .save(&path)
            .expect("a png");
        let first = picture(&path, None).expect("decodes");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!((first.width, first.height), (4096, 82));
        assert_eq!(first.full, (5000, 100));
        assert_eq!(first.serial, 1);

        let model = chat_model();
        let mut app = App::new(florence(), None);
        app.apply(Message::Seen(Box::new(seen_of(&model))));
        assert!(app.viewer.is_none());
        let mut seen = seen_of(&model);
        seen.photo = Some(first.clone());
        app.apply(Message::Seen(Box::new(seen.clone())));
        let viewer = app.viewer.as_ref().expect("the viewer opens");
        // 5000 px on a 1100 px wide window: its own size is past 4x.
        assert!((viewer.zoom.max_scale() - 5000.0 / 1100.0).abs() < 1e-9);
        let _ = app.update(Message::Viewer(viewer::Input::Wheel(
            Point::new(550.0, 360.0),
            3.0,
        )));
        let scale = app.viewer.as_ref().map(|v| v.zoom.scale());
        assert!(scale.is_some_and(|s| s > 1.9), "{scale:?}");
        let esc = Message::Key(keyboard::Event::KeyPressed {
            key: keyboard::Key::Named(keyboard::key::Named::Escape),
            modified_key: keyboard::Key::Named(keyboard::key::Named::Escape),
            physical_key: keyboard::key::Physical::Code(keyboard::key::Code::Escape),
            location: keyboard::Location::Standard,
            modifiers: keyboard::Modifiers::empty(),
            text: None,
            repeat: false,
        });
        let _ = app.update(esc.clone());
        assert!(app.viewer.as_ref().is_some_and(|v| v.zoom.at_rest()));
        let _ = app.update(esc);
        assert!(app.viewer.is_none());
        // The same photo in the next Seen: still closed.
        app.apply(Message::Seen(Box::new(seen.clone())));
        assert!(app.viewer.is_none());
        // A new press, the same photo: open again.
        seen.photo = picture_again(&first);
        app.apply(Message::Seen(Box::new(seen)));
        assert!(app.viewer.is_some());
        let _ = app.update(Message::ViewerClose);
        assert!(app.viewer.is_none());
    }

    fn picture_again(last: &viewer::Picture) -> Option<viewer::Picture> {
        Some(viewer::Picture {
            serial: last.serial + 1,
            ..last.clone()
        })
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

    /// What the window told the model besides the drafts, and the last
    /// draft it told (1219).
    fn told_apart(
        rx: &mut tokio::sync::mpsc::UnboundedReceiver<Want>,
    ) -> (Vec<UiEvent>, Option<UiEvent>) {
        let (drafts, others): (Vec<_>, Vec<_>) = told(rx)
            .into_iter()
            .partition(|e| matches!(e, UiEvent::Draft { .. }));
        (others, drafts.into_iter().last())
    }

    fn type_into(app: &mut App, words: &str) {
        for c in words.chars() {
            let _ = app.update(Message::Compose(text_editor::Action::Edit(
                text_editor::Edit::Insert(c),
            )));
        }
    }

    /// The list and the open chat are the model's scene (1218): the
    /// newest conversation first with its unread count, the open one's
    /// bubbles grouped as the phone groups them, our reaction a chip on
    /// the bubble it names.
    #[test]
    fn the_window_shows_the_models_list_and_chat() {
        let (app, _rx) = chat_window();
        let listed = app.listed();
        let names: Vec<&str> = listed.iter().map(|r| r.row.name.as_str()).collect();
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(names[0].starts_with("Bert"), "{names:?}");
        assert_eq!(listed[0].row.unread, 1);
        let open = app.scene.chat.as_ref().expect("Anna's chat is open");
        assert_eq!(open.peer, ANNA);
        let nodes: Vec<_> = open.groups.iter().flat_map(|g| &g.bubbles).collect();
        assert_eq!(nodes.len(), 4, "the reaction is no bubble");
        let days: Vec<bool> = nodes.iter().map(|n| n.place.day).collect();
        assert_eq!(days, [true, true, false, false]);
        assert!(nodes[2].place.joins_before);
        assert_eq!(nodes[2].bubble.chips.len(), 1);
        assert!(nodes[2].bubble.chips[0].ours);
        assert!(nodes[3].bubble.ours);
        assert_eq!(nodes[3].bubble.mark, Some(loquax::ui::chat::Mark::Sending));
    }

    /// One scene, two targets (1218): the rows the window draws are the
    /// rows the phone's facade lists ([`loquax::ui::scene::rows`] over the
    /// model's order, what `session::found` hands `schema_row`), the same
    /// names in the same order; the search narrows both by one rule. The
    /// control: a model whose order turned round lists them turned round.
    #[test]
    fn the_window_and_the_facade_list_the_same_rows() {
        let model = chat_model();
        let mut app = App::new(Frames::new(loquax::picture::Maps::default()), None);
        app.apply(Message::Seen(Box::new(seen_of(&model))));
        let drawn = |app: &App| -> Vec<String> {
            app.listed().iter().map(|r| r.row.name.clone()).collect()
        };
        let facade = |search: Option<&str>| -> Vec<String> {
            let mut rows = loquax::ui::scene::rows(&model, model.order());
            if let Some(search) = search {
                rows.retain(|r| loquax::ui::chat::row_matches(&r.row, search));
            }
            rows.into_iter().map(|r| r.row.name).collect()
        };
        assert_eq!(drawn(&app), facade(None));
        assert_eq!(drawn(&app).len(), 2);
        let _ = app.update(Message::Search("schulz anna".to_string()));
        assert_eq!(drawn(&app), facade(Some("schulz anna")));
        assert_eq!(drawn(&app).len(), 1);
        let _ = app.update(Message::Search(String::new()));
        let mut turned = model.scene(LayoutClass::Wide);
        if let Some(list) = turned.list.as_mut() {
            list.rows.reverse();
        }
        let mut seen = seen_of(&model);
        seen.scene = turned;
        app.apply(Message::Seen(Box::new(seen)));
        let mut back = facade(None);
        back.reverse();
        assert_eq!(drawn(&app), back);
        assert_ne!(drawn(&app), facade(None));
    }

    /// `Enter` sends what was typed to the open conversation and empties
    /// the field; a reply asked for on a bubble goes as a reply; blank
    /// text sends nothing.
    #[test]
    fn enter_sends_and_a_reply_goes_as_one() {
        let (mut app, mut rx) = chat_window();
        let send = Message::Intent(
            app.scene
                .action(NodeId::Compose)
                .expect("the compose bar sends")
                .intent
                .clone(),
        );
        let _ = app.update(send.clone());
        assert!(told(&mut rx).is_empty(), "nothing typed, nothing sent");
        type_into(&mut app, "on my way");
        let _ = app.update(send.clone());
        assert_eq!(
            told_apart(&mut rx),
            (
                vec![UiEvent::Write {
                    peer: ANNA,
                    body: "on my way".to_string()
                }],
                Some(UiEvent::Draft {
                    peer: ANNA,
                    text: "on my way".to_string(),
                    reply: None
                })
            )
        );
        assert_eq!(app.compose.text().trim(), "");
        let key = bubble_key(&app, [2; 32]);
        let _ = app.update(Message::Intent(Intent::Menu(Some(NodeId::Bubble(key)))));
        let reply = menu_intent(&app, key, |i| matches!(i, Intent::Reply { .. }));
        let _ = app.update(Message::Intent(reply));
        assert_eq!(app.menu, None);
        assert_eq!(app.reply, Some(([2; 32], "Good morning!".to_string())));
        type_into(&mut app, "morning");
        let _ = app.update(send);
        assert_eq!(
            told_apart(&mut rx).0,
            [UiEvent::Reply {
                peer: ANNA,
                target: [2; 32],
                body: "morning".to_string()
            }]
        );
        assert_eq!(app.reply, None);
    }

    /// 1219: the window's field is the open chat's draft. What is typed and
    /// the reply being written go to the model as the draft; another chat
    /// opens with its own field, and coming back puts the text and the
    /// reply back from the scene.
    #[test]
    fn the_field_tells_its_draft_and_takes_it_back_from_the_scene() {
        let mut model = chat_model();
        let (mut app, mut rx) = chat_window();
        let key = bubble_key(&app, [2; 32]);
        let reply = menu_intent(&app, key, |i| matches!(i, Intent::Reply { .. }));
        let _ = app.update(Message::Intent(reply));
        type_into(&mut app, "on my");
        let told = told(&mut rx);
        assert!(told
            .iter()
            .all(|e| matches!(e, UiEvent::Draft { peer: ANNA, .. })));
        for event in told {
            update(&mut model, event);
        }
        update(&mut model, UiEvent::Open { peer: BERT });
        app.apply(Message::Seen(Box::new(seen_of(&model))));
        assert_eq!(app.compose.text().trim(), "", "Bert's field is his own");
        assert_eq!(app.reply, None);
        let anna = app
            .listed()
            .into_iter()
            .find(|r| r.row.peer == ANNA)
            .expect("Anna's row");
        assert_eq!(
            anna.draft.as_ref().map(|d| d.resolve(Lang::En)).as_deref(),
            Some("Draft: on my")
        );
        update(&mut model, UiEvent::Open { peer: ANNA });
        app.apply(Message::Seen(Box::new(seen_of(&model))));
        assert_eq!(app.compose.text().trim_end_matches('\n'), "on my");
        assert_eq!(app.reply, Some(([2; 32], "Good morning!".to_string())));
    }

    /// The key of the bubble whose message is `id`, in the window's scene.
    fn bubble_key(app: &App, id: [u8; 32]) -> u64 {
        app.scene
            .chat
            .iter()
            .flat_map(|c| &c.groups)
            .flat_map(|g| &g.bubbles)
            .find(|n| n.bubble.id == Some(id))
            .map(|n| n.bubble.key)
            .expect("the bubble is in the chat")
    }

    /// The bubble's action the scene offers that `which` picks.
    fn menu_intent(app: &App, key: u64, which: impl Fn(&Intent) -> bool) -> Intent {
        app.scene
            .bubble(NodeId::Bubble(key))
            .and_then(|n| n.actions.iter().find(|a| which(&a.intent)))
            .map(|a| a.intent.clone())
            .expect("the scene offers it")
    }

    /// A click on a row is a hit on its node, which opens its conversation
    /// in the model; a reaction picked on a bubble reacts; a delete asks
    /// once, then deletes.
    #[test]
    fn the_list_opens_and_the_actions_tell_the_model() {
        let (mut app, mut rx) = chat_window();
        let _ = app.update(Message::Intent(Intent::Activate(NodeId::Row(BERT))));
        let react = match menu_intent(&app, bubble_key(&app, [1; 32]), |i| {
            matches!(i, Intent::React { .. })
        }) {
            Intent::React { to, target, .. } => Intent::React {
                to,
                target,
                content: Some("👍".to_string()),
            },
            other => panic!("not a reaction: {other:?}"),
        };
        let _ = app.update(Message::Intent(react));
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
        let key = bubble_key(&app, [1; 32]);
        let ask = menu_intent(&app, key, |i| matches!(i, Intent::Delete { .. }));
        let _ = app.update(Message::Intent(ask.clone()));
        assert_eq!(app.deleting, Some(NodeId::Bubble(key)));
        assert!(told(&mut rx).is_empty(), "the first click only asks");
        let Intent::Delete { messages, .. } = ask else {
            panic!("not a delete");
        };
        let _ = app.update(Message::Intent(Intent::Delete {
            messages,
            confirmed: true,
        }));
        assert_eq!(
            told(&mut rx),
            [UiEvent::Delete {
                peer: ANNA,
                keys: vec![key]
            }]
        );
    }

    /// The new conversation's field opens what an address names and says
    /// what is wrong with anything else.
    #[test]
    fn a_new_conversation_takes_an_address() {
        let (mut app, mut rx) = chat_window();
        let _ = app.update(Message::Intent(Intent::NewConversation));
        let _ = app.update(Message::NewAddress("not hex".to_string()));
        let _ = app.update(Message::NewGo);
        assert!(told(&mut rx).is_empty());
        assert_eq!(
            app.seen.notice.as_deref(),
            Some("That is not a Loquax code.")
        );
        assert!(app.new_address.is_some(), "the field stays for a fix");
        let hex = loquax::address::to_hex(&BERT);
        let _ = app.update(Message::NewAddress(hex));
        let _ = app.update(Message::NewGo);
        assert_eq!(told(&mut rx), [UiEvent::Open { peer: BERT }]);
        assert_eq!(app.new_address, None);
    }

    /// On the chat a key is typing: `+` does not zoom the map behind it,
    /// and the map draws nothing until it is shown. On the map it zooms
    /// through the keymap's `MapCmd`, and `Escape` goes back.
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
        let _ = app.update(Message::Key(plus.clone()));
        assert_eq!(app.view.zoom_milli(), zoom);
        assert!(app.next_job().is_none(), "no frame for a hidden map");
        let _ = app.update(Message::Intent(Intent::ShowMap { peer: None }));
        app.busy = false;
        // The key goes through the keymap, as on the terminal (1218).
        assert_eq!(app.key(&plus), Some(Intent::Map(MapCmd::ZoomIn)));
        let _ = app.update(Message::Key(plus));
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
        let _ = app.update(Message::Key(escape));
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
        let _ = app.update(Message::Intent(Intent::Theme));
        assert!(app.dark());
        let dark = shoot(&app, w, h);
        assert_ne!(light, dark);
        // The rail is the bar's token and the list's ground the
        // background's, in each.
        let px = |v: &[u8], x: u32, y: u32| {
            let i = ((y * w + x) * 3) as usize;
            [v[i], v[i + 1], v[i + 2]]
        };
        let rgb = |argb: u32| {
            let [_, r, g, b] = argb.to_be_bytes();
            [r, g, b]
        };
        let ground = (chat::RAIL as u32 + 4, h - 4);
        for (shot, dark) in [(&light, false), (&dark, true)] {
            let t = chat::tokens(dark);
            assert_eq!(px(shot, 0, 0), rgb(t.bar));
            assert_eq!(px(shot, ground.0, ground.1), rgb(t.background));
        }
        assert_eq!(px(&light, ground.0, ground.1), [0xff, 0xff, 0xff]);
        assert_eq!(px(&dark, ground.0, ground.1), [0x17, 0x18, 0x1b]);
        map::tests::same_as("gui-chat-light.png", &light, w, h);
        map::tests::same_as("gui-chat-dark.png", &dark, w, h);
    }

    /// A photo, a file and a voice message without words (1268): each is
    /// one bubble, its media first and its time (and for ours the mark)
    /// in its last row, no empty text row under it. Pixel for pixel the
    /// golden, light.
    #[test]
    fn media_without_words_is_one_bubble_each() {
        let mut model = crate::shell::Model::new(&[]);
        model.address = Some(OWN);
        model.names.remember(OWN, "Mia Sander");
        model.names.remember(ANNA, "Anna Schulz");
        let carrying = |model: &mut crate::shell::Model, id: u8, fields| {
            update(
                model,
                UiEvent::Engine(OutboxEvent::Received {
                    message_id: [id; 32],
                    source: ANNA,
                    title: Vec::new(),
                    body: Vec::new(),
                    fields,
                    verified: true,
                }),
            );
        };
        update(&mut model, UiEvent::Clock(at(30, 9, 10)));
        carrying(
            &mut model,
            1,
            loquax::ui::photos::photo_fields("jpg", vec![7; 312_400]),
        );
        update(&mut model, UiEvent::Clock(at(30, 9, 20)));
        carrying(
            &mut model,
            2,
            loquax::ui::files::file_fields("report.pdf", vec![7; 1_400]),
        );
        update(&mut model, UiEvent::Clock(at(30, 9, 30)));
        carrying(
            &mut model,
            3,
            loquax::ui::voice::voice_fields(loquax::ui::voice::AM_OPUS_OGG, vec![7; 900]),
        );
        update(&mut model, UiEvent::Open { peer: ANNA });
        update(&mut model, UiEvent::Clock(at(30, 9, 40)));
        update(
            &mut model,
            UiEvent::SendPhoto {
                peer: ANNA,
                format: "jpg".into(),
                data: vec![0xff, 0xd8, 0xff],
            },
        );
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(Frames::new(loquax::picture::Maps::default()), Some(tx));
        app.apply(Message::Seen(Box::new(seen_of(&model))));
        let open = app.scene.chat.as_ref().expect("the chat");
        let bubbles: Vec<_> = open.groups.iter().flat_map(|g| &g.bubbles).collect();
        assert_eq!(bubbles.len(), 4, "one bubble a message");
        assert!(bubbles.iter().all(|b| b.text.is_none() && b.place.meta));
        assert!(bubbles[3].bubble.ours && bubbles[3].mark.is_some());
        let (w, h) = (900, 560);
        app.window = Size::new(w as f32, h as f32);
        map::tests::same_as("gui-chat-media.png", &shoot(&app, w, h), w, h);
    }

    /// A narrow window stacks as the phone does, and a resize switches
    /// (1218): below [`WIDE_FROM`] the open chat is alone, with a back in
    /// its bar and no tabs; the back (and `Escape`, the keymap's Back)
    /// closes it in the model; the list comes with the tabs at the
    /// bottom; a wide window puts the rail, the list and the chat beside
    /// each other again. Pixel for pixel the Compact goldens, light.
    #[test]
    fn a_narrow_window_stacks_and_a_resize_switches() {
        let (w, h) = (420, 640);
        let (mut app, mut rx) = chat_window();
        assert_eq!(app.class(), LayoutClass::Wide);
        let _ = app.update(Message::Window(window::Event::Resized(Size::new(
            w as f32, h as f32,
        ))));
        assert_eq!(app.class(), LayoutClass::Compact);
        assert!(app.scene.list.is_none() && app.scene.tabs.is_none());
        assert_eq!(app.scene.top.id, NodeId::ChatBar);
        map::tests::same_as("gui-chat-compact-chat.png", &shoot(&app, w, h), w, h);
        let _ = app.update(Message::Intent(Intent::Activate(NodeId::TopBar)));
        assert_eq!(told(&mut rx), [UiEvent::Close]);
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
        assert_eq!(app.key(&escape), Some(Intent::Back));
        let _ = app.update(Message::Key(escape));
        assert_eq!(told(&mut rx), [UiEvent::Close]);
        // The model closed it: the list, with the tabs.
        let mut model = chat_model();
        update(&mut model, UiEvent::Close);
        app.apply(Message::Seen(Box::new(seen_of(&model))));
        assert!(app.scene.chat.is_none());
        assert!(app.scene.list.is_some() && app.scene.tabs.is_some());
        map::tests::same_as("gui-chat-compact-list.png", &shoot(&app, w, h), w, h);
        // Wide again: the open chat beside the list.
        app.apply(Message::Seen(Box::new(seen_of(&chat_model()))));
        assert!(app.scene.list.is_none());
        let _ = app.update(Message::Window(window::Event::Resized(Size::new(
            900.0, 560.0,
        ))));
        assert_eq!(app.class(), LayoutClass::Wide);
        assert!(app.scene.list.is_some() && app.scene.chat.is_some());
        assert_eq!(app.scene.top.id, NodeId::TopBar);
    }

    /// The settings page (1233): the gear opens it, its read receipts
    /// row shows the model's switch, a toggle tells the model the other
    /// value, the model's answer turns the row, and Back returns to the
    /// chats. Pixel for pixel the golden, light.
    #[test]
    fn the_settings_page_switches_read_receipts_in_the_model() {
        let (w, h) = (900, 560);
        let (mut app, mut rx) = chat_window();
        app.window = Size::new(w as f32, h as f32);
        let _ = app.update(Message::Settings);
        assert_eq!(app.screen, Screen::Settings);
        assert!(app.seen.receipts, "on by default");
        map::tests::same_as("gui-settings-light.png", &shoot(&app, w, h), w, h);
        let _ = app.update(Message::Intent(Intent::ReadReceipts {
            on: loquax::ui::intent::Switch::Off,
        }));
        assert_eq!(told(&mut rx), [UiEvent::ReadReceipts(false)]);
        let mut model = chat_model();
        let effects = update(&mut model, UiEvent::ReadReceipts(false));
        assert!(matches!(
            effects.as_slice(),
            [loquax::ui::UiEffect::Remember {
                key: "read_receipts",
                value: "off"
            }]
        ));
        app.apply(Message::Seen(Box::new(seen_of(&model))));
        assert!(!app.seen.receipts);
        let _ = app.update(Message::Intent(Intent::ReadReceipts {
            on: loquax::ui::intent::Switch::Toggle,
        }));
        assert_eq!(told(&mut rx), [UiEvent::ReadReceipts(true)]);
        let _ = app.update(Message::Intent(Intent::Back));
        assert_eq!(app.screen, Screen::Chats);
    }

    /// The desktop's dark setting turns the window dark, and the toggle
    /// turns it round either way.
    #[test]
    fn the_theme_follows_the_desktop_and_the_toggle() {
        let (mut app, _rx) = chat_window();
        assert!(!app.dark());
        let _ = app.update(Message::SystemTheme(iced::theme::Mode::Dark));
        assert!(app.dark());
        let _ = app.update(Message::Intent(Intent::Theme));
        assert!(!app.dark());
        let _ = app.update(Message::SystemTheme(iced::theme::Mode::Light));
        assert!(app.dark());
    }
}
