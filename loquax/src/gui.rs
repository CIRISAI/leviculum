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
//!   shown where the view moved it until the next lands; a drag asks for
//!   the next only when the margin left runs short, at most once a frame
//!   interval, and a wheel's notch glides to its zoom over the frame on
//!   screen scaled while the frame at that zoom is drawn (1431); while a drag or
//!   the wheel moves the view the names drawn at rest are kept, and at
//!   rest, with the window focused, the cache prefetches around the view.
//!
//! The map's autoload (1089) runs in the shell: the window decides what to
//! ask with the model's own rules ([`want_pins`] when the pins first arrive,
//! [`want_view`] on `d`, and every frame the core's scheduler,
//! [`ViewLoads`], 1527: the screen at its zoom first) and sends it over; the shell's [`Seen`] carries the
//! loading word and the maps directory's epoch, and a new epoch reads the
//! regions again and paints the view anew.

#[cfg(test)]
mod bench;
pub mod chat;
#[cfg(test)]
mod counts;
/// The first-tile measurement's archive and disk (1527), the phone's too.
#[cfg(test)]
#[path = "../tests/support/first_tile.rs"]
mod first_tile;
#[cfg(test)]
mod idle;
pub mod map;
mod select;
#[cfg(test)]
mod smooth;
pub mod trace;
pub mod viewer;

use std::process::ExitCode;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use iced::advanced::image::{FilterMethod, Handle};
use iced::widget::canvas::{self, Canvas, Frame, Geometry, Path};
use iced::widget::{button, column, container, row, scrollable, stack, text, text_editor, Space};
use iced::{
    keyboard, mouse, window, Color, Element, Length, Point, Rectangle, Renderer, Size,
    Subscription, Task, Theme,
};
use tokio::sync::oneshot;

use leviculum_map::mercator::World;
use leviculum_map::viewport::{ExactView, MILLI};
use leviculum_mapfill::style::Style;
use leviculum_ui::input::KeyCode;
use leviculum_ui::keymap::Ctx;
use loquax::keys::{self, MapCmd};
use loquax::mapframe::{hold, onto, Ask, Frames};
use loquax::picture::map_theme;
use loquax::tiles::Motion;
use loquax::ui::browse::BrowseEvent;
use loquax::ui::compose::Sheet;
use loquax::ui::intent::{Intent, Messages, Target};
use loquax::ui::map::{fit, want_pins, want_view, zoom_floor, MapPin, MapSetup, MapWant};
use loquax::ui::scene::{LayoutClass, NodeId, Scene, TabId};
use loquax::ui::strings::{Lang, StrId};
use loquax::ui::viewload::{ViewLoads, ViewWants};
use loquax::ui::{Pane, UiEvent, MAP_LOADED_ALREADY, MAP_LOADING_HERE_WORDS};
use loquax::when::WallTime;

use crate::desktop;
use crate::shell::{self, local_now, Shell, ShellOptions, PUMP_MS, TICK_MS};

/// The tabs' bar under a narrow window, in logical pixels: what the Browse
/// tab's page leaves it (1369).
const COMPACT_TABS: f32 = 72.0;
/// The list of peers on the left, in logical pixels.
const LIST_WIDTH: f32 = 260.0;
/// The map bar's height: a line of 13 pixel text and its padding (1441).
const MAP_BAR: f32 = 21.0;
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
    /// The fixes the map draws tracks of, shared with the model (1385),
    /// and the window the strip shows chosen.
    pub tracks: loquax::ui::tracks::TrackInput,
    pub track_window: loquax::ui::tracks::TrackWindow,
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
    /// The reader's photo size (1220), the settings' row (1371).
    pub photo_size: loquax::ui::photos::PhotoSize,
    /// The settings' Mailbox section (1450, 1642): the core's mailbox
    /// page, which mailbox, how it is reached, which devices use it.
    pub mailbox: Option<loquax::ui::mailbox_page::MailboxPage>,
    /// The photo a press on a bubble's photo last asked to see whole
    /// (1269), decoded by the shell: a new one opens the viewer.
    pub photo: Option<viewer::Picture>,
    /// The Browse tab while it is in front (1369): the browser the model
    /// hosts, drawn by lnomad's window code.
    pub browse: Option<Browsed>,
    /// The newest text the browser copied, with its serial (1369): a new
    /// one goes to the window's clipboard.
    pub copied: Option<(u64, String)>,
    /// The public rooms' hub lines of the Advanced section (1577), as the
    /// core says them ([`loquax::ui::settings::hub_rows`]).
    pub hubs: Vec<loquax::ui::settings::PageRow>,
}

/// The hosted browser as the window draws it (1369): lnomad's model as
/// the shell holds it, its scene, and the pictures the shell's half of
/// lnomad's picture code took for it.
#[derive(Clone)]
pub struct Browsed {
    pub model: Arc<lnomad::Model>,
    pub scene: lnomad::app::scene::Scene,
    pub pictures: crate::browsing::Pictures,
    /// Moves with every picture taken, so a picture is drawn when it lands.
    pub epoch: u64,
}

impl PartialEq for Browsed {
    /// What the window draws moved: the scene, a picture, the pictures'
    /// slots, the help's scroll, the theme.
    fn eq(&self, other: &Self) -> bool {
        self.scene == other.scene
            && self.epoch == other.epoch
            && self.model.images == other.model.images
            && self.model.help_scroll == other.model.help_scroll
            && self.model.theme == other.model.theme
    }
}

impl std::fmt::Debug for Browsed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Browsed")
            .field("title", &self.model.title)
            .field("epoch", &self.epoch)
            .finish_non_exhaustive()
    }
}

/// The mailbox page (1642), as the phone and the terminal draw it.
fn mailbox_lines(model: &crate::shell::Model) -> Option<loquax::ui::mailbox_page::MailboxPage> {
    Some(loquax::ui::mailbox_page::page(model))
}

impl Seen {
    fn of(shell: &Shell) -> Self {
        Self::of_model(&shell.model, shell.maps.epoch(), &shell.browsing)
    }

    /// What the window shows of `model`, the maps directory at `maps`,
    /// the hosted browser's pictures in `browsing`: [`Seen::of`] without
    /// the rest of the shell, which the idle tests have none of.
    fn of_model(
        model: &crate::shell::Model,
        maps: u64,
        browsing: &crate::browsing::Browsing,
    ) -> Self {
        Self {
            pins: model.map_pins(),
            tracks: model.track_input(),
            track_window: model.track_window,
            notice: model.toasts.last().map(|t| t.text.clone()),
            // The screen's own load in the phone chip's words (1527).
            loading: match model.map_here {
                true => Some(MAP_LOADING_HERE_WORDS.to_string()),
                false => model.map_loading.clone(),
            },
            maps,
            autoload: model.map_autoload,
            style: Style::SCREEN.in_theme(map_theme(model.map_theme, None)),
            scene: model.scene(LayoutClass::Wide),
            now: model.now,
            receipts: model.reading.on,
            photo_size: model.photo_size,
            mailbox: mailbox_lines(model),
            photo: None,
            browse: match (model.browse.showing(), &browsing.pictures) {
                (true, Some(pictures)) => Some(Browsed {
                    model: Arc::new(model.browse.model.clone()),
                    scene: model.browse.model.scene(),
                    pictures: Arc::clone(pictures),
                    epoch: browsing.epoch,
                }),
                _ => None,
            },
            copied: browsing.copied.clone(),
            hubs: loquax::ui::settings::hub_rows(model, loquax::ui::settings::Target::Window),
        }
    }

    /// What [`Seen::of_model`] is built from (1491): the model's
    /// revision, the maps directory's epoch, the browser's pictures and
    /// copies, the photo the viewer shows. The drive builds a state only
    /// when it moved.
    fn stamp(
        model: &crate::shell::Model,
        maps: u64,
        browsing: &crate::browsing::Browsing,
        photo: Option<&viewer::Picture>,
    ) -> u64 {
        loquax::ui::Presented::stamp(
            model.revision,
            &[
                maps,
                browsing.epoch,
                browsing.copied.as_ref().map_or(0, |(serial, _)| *serial),
                photo.map_or(0, |p| p.serial),
            ],
        )
    }
}

/// The drive's half of a pass (1491): the state the window is sent,
/// built only when its stamp moved and sent only when the window would
/// show it differently ([`Sent`]). Before 1491 the drive built the whole
/// scene, the pins and the tracks on every pass, fifty times a second,
/// and [`Sent`] threw the build away.
#[derive(Default)]
struct Driving {
    presented: loquax::ui::Presented,
    sent: Sent,
}

impl Driving {
    /// The state for the window, when one is owed: `build` runs only when
    /// `stamp` moved.
    fn pass(&mut self, stamp: u64, build: impl FnOnce() -> Seen) -> Option<Seen> {
        if !self.presented.due(stamp) {
            return None;
        }
        self.sent.offer(build())
    }

    /// How many states the drive built.
    #[cfg(test)]
    fn builds(&self) -> u64 {
        self.presented.builds()
    }
}

/// What the window asks of the shell: a map region to load, or an event
/// for the model, as a tap or a key on the phone hands it one.
#[derive(Clone, Debug)]
pub enum Want {
    Map(MapWant),
    /// The core scheduler's decision for the map's frame (1527).
    MapView(ViewWants),
    Ui(UiEvent),
    /// A gesture or a button of the recording (1276), timed by the shell
    /// on the clock the model's recording counts by.
    Record {
        peer: [u8; 16],
        input: loquax::ui::recording::Input,
    },
    /// A file the desktop's chooser picked for `peer`'s chat (1341): a
    /// picture to scale or a file to send whole.
    Attach {
        peer: [u8; 16],
        path: std::path::PathBuf,
        photo: bool,
    },
    /// The window gained or lost the focus: a notification only shows
    /// while it has none (1341).
    Front(bool),
}

/// `loquax gui`: the shell started as the terminal starts it, driven on
/// the runtime while the window runs, and finished as the terminal
/// finishes it.
pub async fn run(options: ShellOptions) -> ExitCode {
    let (mut shell, _config) = match shell::start(&options).await {
        Ok(started) => started,
        Err(code) => return code,
    };
    // The window scrolls scenes it keeps whole (1490): the viewport rule
    // reaches it with its own plan step, which wires the scroll offsets
    // into the model; until then it asks for everything.
    shell.model.viewing = loquax::ui::Viewing::ALL;
    // The desktop's portal is the window's picker (1341): Gallery and
    // File in the plus, as on the phone.
    shell.model.bar.abilities.picker = true;
    // No receiver on a desktop (1371): the position sheet takes it by
    // hand, typed or the map's centre.
    shell.model.bar.abilities.manual = true;
    // Set here, not folded: the scene the start built is dropped (1491).
    shell.model.changed_outside();
    // The Browse tab's pictures (1369): lnomad's window code holds them in
    // the shell, which the window draws them from.
    shell.browsing.pictures = Some(Arc::default());
    // The window's map runs the scheduler itself (1527), on its own view.
    shell.map_drive = None;
    let frames = Frames::new(shell.maps.clone());
    let (seen_tx, seen_rx) = unbounded();
    let (asks_tx, asks_rx) = tokio::sync::mpsc::unbounded_channel();
    let (leave_tx, leave_rx) = oneshot::channel();
    let driver = tokio::spawn(drive(shell, leave_rx, seen_tx, asks_rx));
    // iced runs its own loop on this thread until the window closes; the
    // runtime's other threads drive the shell meanwhile.
    let shown = tokio::task::block_in_place(|| window(Some((frames, seen_rx, asks_tx)), false));
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
    // The window speaks the locale's language (as `App::lang` does), so the
    // words the model says itself, a group's notes and a digest's
    // left-out lines among them (1348), are the window's too.
    shell.model.lang = locale();
    shell.model.changed_outside();
    shell.apply(UiEvent::Clock(local_now()));
    let mut clocks = (local_now().unix_secs, shell.elapsed_ms());
    let mut driving = Driving::default();
    let mut photo: Option<viewer::Picture> = None;
    let mut notices = desktop::Notices::default();
    let mut front = true;
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
                // A page's link out of NomadNet, from the Browse tab (1369).
                shell::Output::Open(url) => {
                    if let Err(error) = open::that(&url) {
                        tracing::warn!("could not open {url}: {error}");
                    }
                    continue;
                }
                _ => continue,
            };
            if let Err(error) = open::that(&path) {
                tracing::warn!("could not open {}: {error}", path.display());
            }
        }
        let stamp = Seen::stamp(
            &shell.model,
            shell.maps.epoch(),
            &shell.browsing,
            photo.as_ref(),
        );
        let offered = driving.pass(stamp, || Seen {
            photo: photo.clone(),
            ..Seen::of(&shell)
        });
        if let Some(now) = offered {
            if seen.unbounded_send(now).is_err() {
                break;
            }
            // New mail while the window is in the background (1341): the
            // desktop's notification, as the phone's service posts one.
            let rows = desktop::Count::of(&loquax::ui::chat::list_rows(&shell.model));
            for notice in notices.look(&rows, front, None) {
                tokio::spawn(async move {
                    if let Err(error) = desktop::post(&notice).await {
                        tracing::debug!("no notification: {error}");
                    }
                });
            }
        }
        if shell.model.quit {
            break;
        }
        tokio::select! {
            _ = &mut leave => break,
            Some(want) = asks.recv() => match want {
                Want::Map(want) => shell.map_ask(want),
                Want::MapView(wants) => shell.map_schedule(wants),
                Want::Ui(event) => shell.apply(event),
                Want::Record { peer, input } => shell.record(peer, input),
                Want::Attach { peer, path, photo } => shell.attach(peer, &path, photo),
                Want::Front(is) => front = is,
            },
            _ = pump.tick() => shell.poll(),
            _ = tick.tick() => shell.tick(&mut clocks),
        }
    }
    shell
}

/// What the shell sent the window last (1455): a message wakes iced's
/// loop, which builds the view anew and redraws after every one, so the
/// window is sent only what it would show differently. The model's clock
/// moves every second, and the window shows no second of it: a list's
/// time and the date lines are by the day, a message's time by the
/// minute, the tracks are kept by the minute ([`TrackKey`]), a stale
/// fix by the hour. So the clock counts by the minute here; what else
/// moves each second (a pin's age in words, a recording's length) moves
/// the scene or the pins and is sent.
#[derive(Default)]
struct Sent {
    /// The last state sent, its clock at the start of its minute.
    last: Option<Seen>,
}

impl Sent {
    /// `seen` when the window would show it differently from the last
    /// state sent, its own clock exact; `None` otherwise.
    fn offer(&mut self, mut seen: Seen) -> Option<Seen> {
        let exact = seen.now;
        seen.now = exact.map(|now| WallTime {
            unix_secs: now.unix_secs - now.unix_secs.rem_euclid(60),
            ..now
        });
        if self.last.as_ref() == Some(&seen) {
            return None;
        }
        let out = Seen {
            now: exact,
            ..seen.clone()
        };
        self.last = Some(seen);
        Some(out)
    }
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

/// `loquax gui --exit-after-first-frame` (1342): the window opened on the
/// display there is and closed once its first frame is drawn, without the
/// shell, so no daemon, identity or history is needed. What it proves is
/// the packaged binary's half that a test run cannot: winit finds X11 or
/// Wayland through dlopen and the software renderer puts a frame on it.
/// `scripts/verify-deb-packaging.sh` runs it under `xvfb-run`.
pub fn probe() -> ExitCode {
    match tokio::task::block_in_place(|| window(None, true)) {
        Ok(()) => {
            println!("loquax: the window drew its first frame");
            // With the frame meter on (1441): what iced's own loop timed,
            // which says the meter hears a real window's frames.
            if let Some(frames) = leviculum_ui_iced::fps::snapshot() {
                println!("{}", frames.line());
            }
            ExitCode::SUCCESS
        }
        Err(error) => crate::failure(error, "failed"),
    }
}

/// Measure the window's frames (`--fps` or `LOQUAX_FPS=1`, 1441): the
/// numbers in its top right corner, and a `WINDOW_FRAME` line a second in
/// the log while it draws.
pub fn measure_frames() {
    leviculum_ui_iced::fps::enable(|line| tracing::info!(target: "loquax::window", "{line}"));
}

/// Open the window and run it until it is closed, or, as the packaging
/// probe, until its first frame is drawn. Without the shell's half the
/// window is the empty one [`App::boot`] makes.
fn window(taken: Option<(Frames, UnboundedReceiver<Seen>, Asks)>, probe: bool) -> iced::Result {
    // iced asks its boot function for the first state; it is asked once,
    // and what it needs is handed over through this slot.
    let slot = Mutex::new(taken);
    iced::application(
        move || {
            let taken = slot.lock().unwrap_or_else(PoisonError::into_inner).take();
            let (mut app, task) = App::boot(taken);
            app.probe = probe.then_some(0);
            (app, task)
        },
        App::update,
        App::view,
    )
    .title("Loquax")
    // The Browse tab's page face (1369), as lnomad's own window loads it.
    .font(lnomad::gui::MONO_BYTES)
    .font(lnomad::gui::MONO_BOLD_BYTES)
    .subscription(App::subscription)
    .theme(App::theme)
    .window(iced::window::Settings {
        icon: leviculum_ui_iced::window_icon(),
        // Wayland names the window's icon and its launcher entry by the
        // application id (1341): the desktop file's basename,
        // packaging/loquax/loquax.desktop.
        platform_specific: iced::window::settings::PlatformSpecific {
            application_id: desktop::APP.to_string(),
            ..Default::default()
        },
        ..iced::window::Settings::default()
    })
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
    /// The screen is about to draw a frame (1431): the wheel's glide
    /// moves on, and a frame held back for the frame interval is asked.
    Frame(Instant),
    /// A prefetch batch ended; whether more is to draw.
    Prefetched(bool),
    /// The desktop's light or dark setting, at the start and on a change.
    SystemTheme(iced::theme::Mode),
    /// What the list's search field holds.
    Search(String),
    /// The new conversation's field: what is typed, `Enter`.
    NewAddress(String),
    /// What the new group's name field holds now (1307).
    GroupName(String),
    /// What the Advanced section's hub field holds now (1577).
    HubTyped(String),
    /// The hub field's address taken (its button, or `Enter`).
    HubTake,
    /// What the position sheet's field holds now (1371).
    Position(String),
    /// What the row menu's rename field holds now (1371).
    Renaming(String),
    /// A bubble menu's Forward (1371): pick the conversation.
    ForwardPick(NodeId),
    NewGo,
    Compose(text_editor::Action),
    /// What the user asks (1218): a press on a scene node raises the
    /// node's [`Intent`], a key the keymap's, as on every target.
    Intent(Intent),
    /// The top bar's gear: the settings page (1233).
    Settings,
    /// A "Copy" on the mailbox page (1642): the words onto the clipboard,
    /// and the notice that says so.
    CopyText(String),
    /// A time window chosen in the map's menu (1385, 1419).
    TrackWindow(loquax::ui::tracks::TrackWindow),
    /// The map bar's More: its menu open, or closed again (1419).
    MapMenu,
    /// The map menu's "Time zones" (1684): the time zone view on or off.
    TimeZones,
    /// The time zone view's minute turned: its clocks drawn anew, if the
    /// view is still the one this timer was set for.
    ZoneClock(u64),
    /// The mouse on the photo viewer (1269).
    Viewer(viewer::Input),
    /// The viewer's Close.
    ViewerClose,
    /// The microphone went down under the mouse (1276): it records while
    /// held.
    RecPress,
    /// The pointer, in logical pixels, while the microphone is held.
    RecCursor(Point),
    /// The mouse button came up while the microphone was held.
    RecRelease,
    /// The pointer went onto the lock chip, or off it, while held.
    RecOverLock(bool),
    /// The map list's "Add" on a node nearby that is no contact (1302).
    AddNearby([u8; 16]),
    /// The desktop's chooser answered for `peer`'s chat (1341): the path,
    /// none when cancelled, or why there was no chooser.
    Picked {
        peer: [u8; 16],
        photo: bool,
        path: Result<Option<std::path::PathBuf>, String>,
    },
    /// The new conversation's "Read a code from a picture" (1341): the
    /// chooser for a picture of a contact code.
    PickCode,
    /// The chooser's answer for it.
    CodePicked(Result<Option<std::path::PathBuf>, String>),
    /// A frame is due while the packaging probe runs ([`probe`]).
    Probed,
    /// The Browse tab's view said this (1369): lnomad's window message,
    /// for the browser the model hosts.
    Browse(lnomad::gui::Message),
    /// The Rooms tab said this (1379): the input's text, its Enter, a
    /// room chosen.
    Rooms(loquax::ui::rooms::RoomsEvent),
    /// The Explore tab said this (1578): the search field's text, the
    /// order, a page, a bookmark, a room, a hub.
    Explore(loquax::ui::explore::ExploreEvent),
    /// The left button on a text that selects (1444, 1475): a press, a
    /// drag over it, the release.
    PickText(
        loquax::ui::selection::RunKey,
        leviculum_ui_iced::select::Pick,
    ),
    /// A middle click on the compose field (1444): paste the PRIMARY
    /// selection where it clicked (1475).
    PastePrimary,
    /// The pointer over the compose field, in the field (1475): where a
    /// middle click pastes.
    ComposePointer(iced::Point),
    /// What the PRIMARY selection held, for the compose field.
    PrimaryText(Option<String>),
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
                | Message::Frame(_)
                | Message::Prefetched(_)
                | Message::Settings
                | Message::Viewer(_)
                | Message::ViewerClose
                | Message::AddNearby(_)
                | Message::Picked { .. }
                | Message::PickCode
                | Message::CodePicked(_)
                | Message::Probed
                | Message::Browse(_)
                | Message::Rooms(_)
                | Message::Explore(_)
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
    /// The Browse tab (1369): lnomad's window over the browser the model
    /// hosts, while the model has it in front.
    Browse,
    /// The Rooms tab (1379), while the model has it in front.
    Rooms,
    /// The Explore tab's list (1578), while the model has it in front.
    Explore,
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

/// A frame interval of the screen: 60 frames a second. While the view
/// moves, the worker is asked for a frame at most once in it (1431).
const FRAME: Duration = Duration::from_micros(16_667);
/// How long a wheel's notch glides from the zoom on screen to the zoom it
/// asks (1431), as the phone's pinch follows the fingers: eased out.
const GLIDE_MS: u64 = 150;

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
    /// The tracks at the view's zoom (1385).
    pub tracks: Vec<loquax::ui::tracks::Track>,
    /// The radar where no region touches the view (1386).
    pub radar: Option<loquax::ui::radar::Radar>,
    pub selected: Option<[u8; 16]>,
    pub zooms: (u8, u8),
    /// The maps directory's epoch it is asked at.
    pub epoch: u64,
    pub motion: Motion,
    /// The time zone view at this minute, in seconds since the epoch
    /// (1684); `None` without it.
    pub time_zones: Option<i64>,
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
    /// What drawing it took on the worker, in milliseconds (1431).
    pub ms: f64,
}

/// What the worker does next.
#[derive(Debug)]
enum Job {
    Draw(Box<Order>),
    Prefetch(u64),
}

/// `order` drawn through the map's frames ([`loquax::mapframe`], the
/// phone's pipeline): the tiles composed at the exact zoom, also while
/// the wheel's glide scales the frame on screen towards it (1431), the
/// names placed at rest and kept while moving, the pins placed with the
/// window's font and left for its canvas.
pub fn draw(frames: &Mutex<Frames>, order: Order) -> Arc<Shown> {
    let start = Instant::now();
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
        tracks: &order.tracks,
        radar: order.radar.as_ref(),
        selected: order.selected,
        zooms: order.zooms,
        draw_pins: false,
        measure: Some(map::measure),
        now_ms: Some(now_ms),
        // The wheel's glide asks for the frame at the zoom it ends on
        // (1431): drawn there, at the exact zoom, the glide scales it the
        // rest of the way and ends on it sharp. A pan stays at its zoom.
        scaled_while_moving: false,
        time_zones: order.time_zones,
    });
    let (w, h) = shot.view.size();
    Arc::new(Shown {
        handle: Handle::from_rgba(w, h, shot.rgba),
        view: shot.view,
        placed: frames.placed().to_vec(),
        attribution: shot.attribution,
        missing: shot.missing,
        fading: shot.fading,
        ms: start.elapsed().as_secs_f64() * 1e3,
        order,
    })
}

/// Milliseconds until the next minute begins, and a little past it.
fn to_next_minute() -> u64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    u64::try_from(60_000 - now % 60_000).unwrap_or(60_000) + 50
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

/// A wheel's zoom on its way (1431): from the view on screen when the
/// notch came to the view the notches ask, over [`GLIDE_MS`], eased out.
#[derive(Clone, Copy, Debug)]
struct Glide {
    from: ExactView,
    to: ExactView,
    start: Instant,
}

impl Glide {
    /// The view on screen at `now`, and whether the glide is over. The
    /// zoom moves evenly in steps (the scale by its logarithm) about the
    /// one screen point the two views share, so what is under the
    /// pointer stays under it all the way.
    fn at(&self, now: Instant) -> (ExactView, bool) {
        let t = now.saturating_duration_since(self.start).as_secs_f64() * 1e3 / GLIDE_MS as f64;
        if t >= 1.0 {
            return (self.to, true);
        }
        let (x, y, k) = onto(&self.from, &self.to);
        if (1.0 - k).abs() < 1e-6 {
            return (self.to, true);
        }
        let (px, py) = (x / (1.0 - k), y / (1.0 - k));
        let eased = 1.0 - (1.0 - t).powi(3);
        let (a, b) = (
            f64::from(self.from.zoom_milli()),
            f64::from(self.to.zoom_milli()),
        );
        let mut view = self.from;
        view.zoom_about((a + (b - a) * eased).round() as u32, px, py);
        (view, false)
    }
}

/// The window's state.
/// What the window's tracks were derived at ([`App::tracks`]).
#[derive(Clone, Debug, PartialEq)]
struct TrackKey {
    zoom: u8,
    scale: u32,
    window: loquax::ui::tracks::TrackWindow,
    minute: i64,
    fixes: Vec<([u8; 16], usize, usize)>,
}

struct App {
    seen: Seen,
    /// The tracks last derived, and at what.
    track_cache: std::cell::RefCell<Option<(TrackKey, Vec<loquax::ui::tracks::Track>)>>,
    asks: Option<Asks>,
    /// The core's scheduler over the map's frames (1527): the screen at its
    /// zoom loads first.
    loads: ViewLoads,
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
    /// The wheel's zoom on its way (1431).
    glide: Option<Glide>,
    /// When the worker was last asked for a frame, and whether one is
    /// held back for the frame interval (1431).
    asked_at: Option<Instant>,
    owed: bool,
    /// What the map did per event and per frame (1431).
    trace: trace::Trace,
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
    /// The Advanced section's field for another hub's address (1577).
    hub_field: String,
    /// The position sheet's field while it is open (1371).
    position: Option<String>,
    /// The row menu's rename field while it is open (1371): whose, and
    /// what it holds.
    renaming: Option<([u8; 16], String)>,
    /// The map was shown once: the position sheet offers its centre.
    map_shown: bool,
    /// The keys' help over the window while it is open (1371), at this
    /// line.
    help: Option<usize>,
    /// The map's menu over the map while it is open (1419): the tracks'
    /// time window and "fit all".
    map_menu: bool,
    /// The time zone view (1684): the zones' borders and each one's clock
    /// over the map, from the map's menu.
    time_zones: bool,
    /// Which switching-on of the time zone view the minute's timer
    /// belongs to: an older timer finds another number and stops.
    zone_clock: u64,
    /// The bubble whose Forward was pressed (1371): its menu lists the
    /// conversations to send the copy to.
    forwarding: Option<NodeId>,
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
    /// The text selected in the open chat (1444), and whether the left
    /// button that began it is still held.
    picked: Option<loquax::ui::selection::TextSelection>,
    picking: bool,
    /// The release of a selection's drag on a text in a button (a list
    /// row, a position line) came (1475): the button's open, which the
    /// same release sends right after, is dropped. Reset by every view,
    /// so it never outlives the event that set it.
    drop_open: std::cell::Cell<bool>,
    /// The pointer's last place over the compose field (1475).
    compose_pointer: Option<iced::Point>,
    /// The test clipboard (1444): what went onto which buffer.
    #[cfg(test)]
    clips: Vec<(select::Buffer, String)>,
    /// The settings' Maps rows (1290): how many regions and their bytes,
    /// read as the page opens and after a delete, and whether "Delete
    /// all downloaded maps" was asked once.
    maps_usage: (usize, u64),
    maps_asked: bool,
    /// The settings' open section (1450), none for their list.
    settings_at: Option<loquax::ui::settings::Section>,
    /// The language the scene's words are said in: the desktop's locale.
    lang: Lang,
    /// The scene of [`Seen`] in the window's class: what is drawn.
    scene: Scene,
    /// The photo whole while it is open (1269), over everything.
    viewer: Option<viewer::Viewer>,
    /// The microphone held by the mouse (1276): the chat it records for,
    /// where the pointer was first seen since, and whether it is over
    /// the lock chip.
    rec_held: Option<[u8; 16]>,
    rec_origin: Option<Point>,
    rec_over_lock: bool,
    /// The packaging probe's frames so far ([`probe`]); `None` in a window
    /// a person uses.
    probe: Option<u32>,
    /// The reader left the Browse tab (1369) and the model has not said
    /// so yet: a [`Seen`] from before still holds the browser.
    browse_left: bool,
    /// The reader left the Rooms tab (1379) and the model has not said so
    /// yet, as [`App::browse_left`].
    rooms_left: bool,
    /// The reader left the Explore tab (1578), as [`App::browse_left`].
    explore_left: bool,
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
        let theme = leviculum_ui_iced::desktop::theme().map(Message::SystemTheme);
        (app, Task::batch([scale, seen, theme]))
    }

    fn new(frames: Frames, asks: Option<Asks>) -> Self {
        let setup = map::setup(frames.painter());
        let centre = fit(&[], map::area((1, 1)), &setup).0;
        let empty = crate::shell::Model::new(&[]).scene(LayoutClass::Wide);
        Self {
            seen: Seen {
                pins: Vec::new(),
                tracks: Vec::new(),
                track_window: Default::default(),
                notice: None,
                loading: None,
                maps: 0,
                autoload: false,
                style: Style::SCREEN,
                scene: empty.clone(),
                now: None,
                receipts: true,
                photo_size: loquax::ui::photos::PhotoSize::default(),
                mailbox: None,
                photo: None,
                browse: None,
                copied: None,
                hubs: Vec::new(),
            },
            asks,
            track_cache: std::cell::RefCell::new(None),
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
            glide: None,
            asked_at: None,
            owed: false,
            trace: trace::Trace::from_env(),
            focused: true,
            prefetching: false,
            motion: Motion::default(),
            loads: ViewLoads::new(),
            origin: Instant::now(),
            screen: Screen::Chats,
            window: Size::new(WINDOW.0, WINDOW.1),
            system_dark: false,
            flipped: false,
            search: String::new(),
            new_address: None,
            hub_field: String::new(),
            position: None,
            renaming: None,
            map_shown: false,
            help: None,
            map_menu: false,
            time_zones: false,
            zone_clock: 0,
            forwarding: None,
            compose: text_editor::Content::new(),
            reply: None,
            compose_for: None,
            emoji: false,
            menu: None,
            deleting: None,
            picked: None,
            picking: false,
            drop_open: std::cell::Cell::new(false),
            compose_pointer: None,
            #[cfg(test)]
            clips: Vec::new(),
            maps_usage: (0, 0),
            maps_asked: false,
            settings_at: None,
            lang: Lang::En,
            scene: empty,
            viewer: None,
            rec_held: None,
            rec_origin: None,
            rec_over_lock: false,
            probe: None,
            browse_left: false,
            rooms_left: false,
            explore_left: false,
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

    /// The window gained or lost the focus: it prefetches only with it,
    /// and the shell notifies only without it (1341).
    fn front(&mut self, focused: bool) {
        self.focused = focused;
        if let Some(asks) = &self.asks {
            let _ = asks.send(Want::Front(focused));
        }
    }

    /// The desktop's chooser for `peer`'s chat, the sheet closed (1341).
    fn pick(&self, peer: [u8; 16], what: desktop::Pick, title: StrId) -> Task<Message> {
        self.tell(UiEvent::CloseSheet);
        let title = title.text(self.lang);
        Task::perform(
            async move { desktop::pick(title, what).await },
            move |path| Message::Picked {
                peer,
                photo: what == desktop::Pick::Picture,
                path,
            },
        )
    }

    /// Hand a gesture or a button of the recording to the model, through
    /// the shell's clock (1276).
    fn record(&self, peer: [u8; 16], input: loquax::ui::recording::Input) {
        if let Some(asks) = &self.asks {
            let _ = asks.send(Want::Record { peer, input });
        }
    }

    /// The microphone held with the mouse (1276): down is the press,
    /// every move the slide from where the pointer was first seen, in
    /// logical pixels as dp (left and up negative), up the release, or
    /// the lock where it comes up over the chip.
    fn rec_mouse(&mut self, message: Message) {
        use loquax::ui::recording::{Input, Verb};
        match message {
            Message::RecPress => {
                let Some(peer) = self.scene.chat.as_ref().map(|c| c.peer) else {
                    return;
                };
                self.rec_held = Some(peer);
                self.rec_origin = None;
                self.rec_over_lock = false;
                self.record(peer, Input::Press { now: 0 });
            }
            Message::RecCursor(at) => {
                let Some(peer) = self.rec_held else {
                    return;
                };
                let origin = *self.rec_origin.get_or_insert(at);
                self.record(
                    peer,
                    Input::Slide {
                        dx: (at.x - origin.x).round() as i32,
                        dy: (at.y - origin.y).round() as i32,
                    },
                );
            }
            Message::RecRelease => {
                let Some(peer) = self.rec_held.take() else {
                    return;
                };
                match std::mem::take(&mut self.rec_over_lock) {
                    true => self.record(peer, Input::Verb(Verb::Lock)),
                    false => self.record(peer, Input::Release { now: 0 }),
                }
            }
            Message::RecOverLock(over) => self.rec_over_lock = over && self.rec_held.is_some(),
            _ => {}
        }
    }

    /// What a chat message changes; a task for what iced does for it.
    fn chat(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::SystemTheme(mode) => self.system_dark = mode == iced::theme::Mode::Dark,
            Message::Search(query) => self.search = query,
            Message::NewAddress(typed) => self.new_address = Some(typed),
            Message::Position(typed) => self.position = Some(typed),
            Message::ForwardPick(id) => self.forwarding = Some(id),
            Message::Renaming(typed) => {
                if let Some((_, name)) = self.renaming.as_mut() {
                    *name = typed;
                }
            }
            Message::GroupName(typed) => {
                self.tell(UiEvent::Group(loquax::ui::groups::Verb::Name(typed)));
            }
            Message::HubTyped(typed) => self.hub_field = typed,
            Message::HubTake => {
                let typed = core::mem::take(&mut self.hub_field);
                if !typed.trim().is_empty() {
                    self.tell(UiEvent::Rooms(loquax::ui::rooms::RoomsEvent::Hub(
                        loquax::ui::rooms::HubChoice::Address(typed),
                    )));
                }
            }
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
            Message::RecPress
            | Message::RecCursor(_)
            | Message::RecRelease
            | Message::RecOverLock(_) => self.rec_mouse(message),
            Message::PickText(key, pick) => return self.pick_text(key, pick),
            Message::PastePrimary => return self.paste_primary(),
            Message::ComposePointer(at) => self.compose_pointer = Some(at),
            Message::PrimaryText(text) => self.primary_text(text),
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
        // A selection is the open chat's (1444).
        self.picked = None;
        self.picking = false;
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
            // The plus's sheet and the position sheet (1270) are the
            // model's; the window draws them from the scene.
            Intent::Attach { peer } => self.tell(UiEvent::OpenSheet(Sheet::Attach { peer })),
            Intent::Locate { peer } => {
                self.tell_map_centre();
                self.tell(UiEvent::OpenSheet(Sheet::Position { peer }));
            }
            Intent::CloseSheet => {
                self.position = None;
                self.tell(UiEvent::CloseSheet);
            }
            // The position by hand (1371): the sheet's field, then what
            // it holds goes to the model, which reads it.
            Intent::EnterPosition { text: None } => {
                self.position = Some(String::new());
                return iced::widget::operation::focus(chat::POSITION_FIELD);
            }
            Intent::EnterPosition { text: Some(text) } => {
                self.position = None;
                self.tell(UiEvent::EnterPosition { text });
            }
            Intent::FixAtMapCentre => {
                self.position = None;
                self.tell_map_centre();
                self.tell(UiEvent::FixAtMapCentre);
            }
            // Gallery and File: the desktop's chooser picks (1341).
            Intent::SendPhoto { peer, .. } => {
                return self.pick(peer, desktop::Pick::Picture, StrId::AttachPhoto)
            }
            Intent::SendFile { peer, .. } => {
                return self.pick(peer, desktop::Pick::File, StrId::AttachFile)
            }
            // The keys' help (1371), the terminal's overlay in the window
            // as lnomad's window draws it; Back or a click closes it.
            Intent::Help => self.help = Some(0),
            Intent::Back | Intent::Dismiss if self.help.is_some() => self.help = None,
            // The profile (1443): a click on the avatar, the settings' row
            // and the palette's `my code` open the scene's profile, as a
            // tap on the avatar does on the phone; Back closes it.
            Intent::Profile => self.tell(UiEvent::Profile { open: true }),
            Intent::Back | Intent::Dismiss if self.scene.profile.is_some() => {
                self.tell(UiEvent::Profile { open: false })
            }
            Intent::Back | Intent::Dismiss if self.map_menu => self.map_menu = false,
            // "Share my address": the desktop's share is its clipboard.
            Intent::ShareCode => {
                if let Some(code) = self.scene.profile.as_ref().and_then(|p| p.code.clone()) {
                    self.seen.notice = Some(StrId::ProfileCopied.text(self.lang).to_string());
                    return iced::clipboard::write(code);
                }
            }
            Intent::Scroll(cmd) if self.help.is_some() => {
                let (viewport, max) =
                    leviculum_ui::help::scroll_bounds(&loquax::ui::help_groups(), self.help_area());
                let at = self.help.unwrap_or_default();
                self.help = Some(leviculum_ui::keymap::scrolled(at, cmd, viewport, max));
            }
            Intent::PhotoSize(size) => self.tell(UiEvent::PhotoSize(size)),
            Intent::Back | Intent::Dismiss if self.menu.is_none() && self.scene.sheet.is_some() => {
                self.tell(UiEvent::CloseSheet)
            }
            Intent::Back | Intent::Dismiss => match (self.menu, self.screen) {
                (Some(_), _) => {
                    self.menu = None;
                    self.deleting = None;
                }
                (None, Screen::Settings) if self.maps_asked => self.maps_asked = false,
                // A section goes back to the settings' list (1450).
                (None, Screen::Settings) if self.settings_at.is_some() => self.settings_at = None,
                (None, Screen::Map | Screen::Settings) => self.screen = Screen::Chats,
                // The browser and a hub's rooms go back to Explore, which
                // opened them (1578); Explore goes back to the chats.
                (None, Screen::Browse | Screen::Rooms) => {
                    self.browse_left = true;
                    self.rooms_left = true;
                    self.explore_left = false;
                    self.tell(UiEvent::Tab(TabId::Explore));
                }
                (None, Screen::Explore) => {
                    self.leave_browse();
                    self.screen = Screen::Chats;
                }
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
            // The settings (1450): the gear's list, a section from it.
            // Profile is the avatar's layer, opened over the list.
            Intent::Settings(at) => {
                self.screen = Screen::Settings;
                self.menu = None;
                self.maps_asked = false;
                match at {
                    Some(loquax::ui::settings::Section::Profile) => {
                        self.settings_at = None;
                        self.tell(UiEvent::Profile { open: true });
                    }
                    _ => {
                        self.settings_at = at;
                        self.maps_usage = self.maps().usage();
                    }
                }
            }
            // The group verbs (1307) are the model's own; the window draws
            // the picker and the sheet from the scene.
            Intent::Group(verb) => {
                // "New group" is reached from the new chat's field.
                if verb == loquax::ui::groups::Verb::New {
                    self.new_address = None;
                }
                self.tell(UiEvent::Group(verb));
            }
            Intent::ReadReceipts { on } => {
                self.tell(UiEvent::ReadReceipts(on.apply(self.seen.receipts)));
            }
            // The public rooms' hub (1577): a shipped one or none at once,
            // another from the field, which a press on its line focuses.
            Intent::RoomsHub(pick) => {
                use loquax::ui::rooms::{HubChoice, HubPick, RoomsEvent};
                let choice = match pick {
                    HubPick::Hub(hub) => HubChoice::Hub(hub),
                    HubPick::None => HubChoice::None,
                    HubPick::Paste => {
                        return iced::widget::operation::focus(chat::HUB_FIELD);
                    }
                    // "More…" (1599): every hub, in the page's scroll.
                    HubPick::More => {
                        self.tell(UiEvent::Rooms(RoomsEvent::More));
                        return Task::none();
                    }
                };
                self.tell(UiEvent::Rooms(RoomsEvent::Hub(choice)));
            }
            Intent::DeleteAllRegions { confirmed: false } => {
                self.maps_usage = self.maps().usage();
                self.maps_asked = self.maps_usage.0 > 0;
            }
            Intent::DeleteAllRegions { confirmed: true } => {
                self.maps_asked = false;
                if let Err(why) = self.maps().delete_all() {
                    self.seen.notice = Some(why);
                }
                self.regions_changed();
                self.maps_usage = self.maps().usage();
                return self.paint();
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
                self.map_shown = true;
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
            Intent::Tab(TabId::Map) => {
                self.leave_browse();
                return self.intent(Intent::ShowMap { peer: None });
            }
            Intent::Tab(TabId::Chats) => {
                self.leave_browse();
                self.screen = Screen::Chats;
            }
            // The Browse tab (1369): the model brings it to the front, and
            // the window follows what it says.
            Intent::Tab(TabId::Browse) => {
                self.browse_left = false;
                self.tell(UiEvent::Tab(TabId::Browse));
            }
            // The Rooms tab (1379): as the Browse tab, the model brings it.
            Intent::Tab(TabId::Rooms) => {
                self.rooms_left = false;
                self.tell(UiEvent::Tab(TabId::Rooms));
            }
            // Explore (1578): the model brings its list to the front, and
            // what it opens (the browser, a hub's rooms) the window follows.
            Intent::Tab(TabId::Explore) => {
                self.explore_left = false;
                self.browse_left = false;
                self.rooms_left = false;
                self.tell(UiEvent::Tab(TabId::Explore));
            }
            Intent::Browse(intent) => self.tell(UiEvent::Browse(BrowseEvent::Intent(intent))),
            Intent::Explore(said) => {
                self.browse_left = false;
                self.rooms_left = false;
                self.tell(UiEvent::Explore(said));
            }
            Intent::Menu(id) => {
                self.menu = id;
                self.deleting = None;
                self.renaming = None;
                self.forwarding = None;
            }
            // A copy of the message to the conversation picked (1371).
            Intent::Forward { from, key, to } => {
                self.menu = None;
                self.forwarding = None;
                self.tell(UiEvent::Forward { from, key, to });
            }
            // A row menu's verbs on a peer (1371): the model's, as the
            // terminal's palette does them; a forget asks once, a rename
            // opens its field.
            Intent::Forget {
                peer: Target::Peer(peer),
                confirmed: false,
                ..
            } => self.deleting = Some(NodeId::Row(peer)),
            Intent::Rename {
                peer: Target::Peer(peer),
                name: None,
            } => {
                let name = self
                    .scene
                    .list
                    .iter()
                    .flat_map(|list| &list.rows)
                    .find(|r| r.id == NodeId::Row(peer))
                    .map(|r| r.row.name.clone())
                    .unwrap_or_default();
                self.renaming = Some((peer, name));
            }
            word @ (Intent::Pin { .. }
            | Intent::Mute { .. }
            | Intent::Archive { .. }
            | Intent::Block { .. }
            | Intent::Verify { .. }
            | Intent::MarkUnread { .. }
            | Intent::Rename { .. }
            | Intent::Forget { .. }) => {
                self.menu = None;
                self.deleting = None;
                self.renaming = None;
                if let Some((peer, verb)) = loquax::ui::verbs::PeerVerb::of(&word) {
                    self.tell(UiEvent::PeerWord { peer, verb });
                }
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
            // The recording bar's buttons (1276).
            Intent::Recording { peer, verb } => {
                self.record(peer, loquax::ui::recording::Input::Verb(verb))
            }
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
            Intent::Map(MapCmd::TimeZones) if self.screen == Screen::Map => {
                return self.update(Message::TimeZones);
            }
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

    /// The map's centre, once the map was shown, for the position sheet
    /// (1371).
    fn tell_map_centre(&self) {
        if self.map_shown {
            let now = self.seen.now.map_or(0, |now| now.unix_secs);
            let centre = loquax::ui::by_hand::of_world(self.view.centre, now);
            self.tell(UiEvent::MapCentre(Some(centre)));
        }
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
        self.settle();
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
            MapCmd::FitWindow => {
                self.map_menu = false;
                self.fit_window();
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
                    // A node nearby is added, which opens its chat (1302).
                    let nearby = self.seen.pins.iter().any(|p| p.peer == peer && p.nearby);
                    self.tell(match nearby {
                        true => UiEvent::AddNearby { peer },
                        false => UiEvent::Open { peer },
                    });
                }
            }
            MapCmd::Close => {
                self.map_menu = false;
                self.screen = Screen::Chats;
            }
            MapCmd::TrackWindow(forward) => {
                let next = self.seen.track_window.step(forward);
                self.tell(UiEvent::TrackWindow(next));
            }
            // The time zone view is the update's, which sets its timer.
            MapCmd::Drawing(_) | MapCmd::GlyphTest | MapCmd::TimeZones => {}
        }
    }

    /// The intent the keymap gives a key the widgets left alone (1218):
    /// the same chords as the terminal's ([`keys::resolve`]), on the map
    /// the map's, on a stacked chat the messages' (`Escape` is Back), on
    /// the chats the list's.
    fn key(&self, event: &keyboard::Event) -> Option<Intent> {
        let key = leviculum_ui_iced::key::chord(event)?;
        let pane = match (self.screen, self.scene.top.id) {
            (Screen::Map, _) => Pane::Map,
            // The Rooms' input field has the keys while it is in front,
            // and Explore's search field (1578).
            (Screen::Browse | Screen::Rooms | Screen::Explore, _) => return None,
            (Screen::Chats, NodeId::ChatBar) => Pane::Messages,
            (Screen::Chats | Screen::Settings, _) => Pane::Conversations,
        };
        keys::resolve(&key, keys::panes_of(pane), Ctx::ENHANCED).map(Intent::from_action)
    }

    fn subscription(&self) -> Subscription<Message> {
        let mut all = vec![
            window::events().map(|(_, event)| Message::Window(event)),
            keyboard::listen().map(Message::Key),
            leviculum_ui_iced::desktop::theme_changes().map(Message::SystemTheme),
        ];
        // The pointer anywhere in the window while the microphone is held
        // (1276): it may leave the button, and the release counts
        // wherever it is.
        if self.rec_held.is_some() {
            all.push(iced::event::listen_with(held_pointer));
        }
        if self.probe.is_some() {
            all.push(window::frames().map(|_| Message::Probed));
        }
        // The screen's frames while the wheel's zoom glides or a frame is
        // held back for the frame interval (1431).
        if self.screen == Screen::Map && (self.animating() || self.owed) {
            all.push(window::frames().map(Message::Frame));
        }
        Subscription::batch(all)
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
        // What the Browse tab copied goes to the window's clipboard (1369).
        let copy = match &message {
            Message::Seen(seen) if seen.copied.is_some() && seen.copied != self.seen.copied => seen
                .copied
                .as_ref()
                .map(|(_, text)| iced::clipboard::write(text.clone())),
            _ => None,
        };
        // The open a selection's drag on a button's text ended in (1475).
        if self.drop_open.replace(false) && matches!(message, Message::Intent(_)) {
            return copy.unwrap_or_else(Task::none);
        }
        let start = Instant::now();
        let task = self.update_inner(message);
        self.browse_sync();
        self.rooms_sync();
        self.explore_sync();
        leviculum_ui_iced::fps::update(start, start.elapsed());
        match copy {
            Some(copy) => Task::batch([copy, task]),
            None => task,
        }
    }

    /// The window's screen follows the model's Browse tab (1369): in front
    /// while the model has it so, unless the reader just left it; the
    /// browser told the window's light or dark and the page's cells.
    fn browse_sync(&mut self) {
        match (&self.seen.browse, self.screen) {
            (None, _) => {
                self.browse_left = false;
                if self.screen == Screen::Browse {
                    self.screen = Screen::Chats;
                }
            }
            (Some(_), Screen::Browse) => {}
            (Some(_), _) if self.browse_left => {}
            (Some(_), _) => {
                self.screen = Screen::Browse;
                self.menu = None;
            }
        }
        let Some(browsed) = self
            .seen
            .browse
            .as_ref()
            .filter(|_| self.screen == Screen::Browse)
        else {
            return;
        };
        let theme = match self.dark() {
            true => lnomad::Theme::Dark,
            false => lnomad::Theme::Light,
        };
        let mut said = Vec::new();
        if browsed.model.theme != theme {
            said.push(BrowseEvent::Theme(theme));
        }
        let (cols, rows) = self.browse_cells();
        if browsed.model.size != (cols, rows) {
            said.push(BrowseEvent::Resize { cols, rows });
        }
        for event in said {
            self.tell(UiEvent::Browse(event));
        }
    }

    /// The Browse tab's page in cells: the window less the tabs' rail or
    /// bar, as lnomad's window counts its own.
    fn browse_cells(&self) -> (u16, u16) {
        match self.class() {
            LayoutClass::Wide => {
                lnomad::gui::cells(self.window.width - chat::RAIL, self.window.height)
            }
            LayoutClass::Compact => {
                lnomad::gui::cells(self.window.width, self.window.height - COMPACT_TABS)
            }
        }
    }

    /// The reader left the Browse tab for another: the model is told, and
    /// the window does not follow a [`Seen`] from before back into it.
    fn leave_browse(&mut self) {
        if self.screen == Screen::Browse || self.seen.browse.is_some() {
            self.browse_left = true;
            self.tell(UiEvent::Tab(TabId::Chats));
        }
        if self.screen == Screen::Rooms || self.seen.scene.rooms.is_some() {
            self.rooms_left = true;
            self.tell(UiEvent::Tab(TabId::Chats));
        }
        if self.screen == Screen::Explore || self.seen.scene.explore.is_some() {
            self.explore_left = true;
            self.tell(UiEvent::Tab(TabId::Chats));
        }
    }

    /// The window follows the model's Explore list (1578), but for a
    /// [`Seen`] from before the reader left it; the browser and a hub's
    /// rooms it opens are their own screens ([`App::browse_sync`],
    /// [`App::rooms_sync`]).
    fn explore_sync(&mut self) {
        match (self.seen.scene.explore.is_some(), self.screen) {
            (false, _) => {
                self.explore_left = false;
                if self.screen == Screen::Explore {
                    self.screen = Screen::Chats;
                }
            }
            (true, Screen::Explore) => {}
            (true, _) if self.explore_left => {}
            (true, _) => {
                self.screen = Screen::Explore;
                self.menu = None;
            }
        }
    }

    /// The window follows the model's Rooms tab (1379), but for a [`Seen`]
    /// from before the reader left it.
    fn rooms_sync(&mut self) {
        match (self.seen.scene.rooms.is_some(), self.screen) {
            (false, _) => {
                self.rooms_left = false;
                if self.screen == Screen::Rooms {
                    self.screen = Screen::Chats;
                }
            }
            (true, Screen::Rooms) => {}
            (true, _) if self.rooms_left => {}
            (true, _) => {
                self.screen = Screen::Rooms;
                self.menu = None;
            }
        }
    }

    /// What lnomad's window view said, for the browser the model hosts
    /// (1369), as lnomad's own window folds it: a press its intent, the
    /// address field's text and Enter, the wheel's notches and the
    /// pointer's cell as the model's mouse.
    fn browse(&self, message: lnomad::gui::Message) {
        use lnomad::gui::Message as Said;
        let events = match message {
            Said::Intent(intent) => vec![UiEvent::Browse(BrowseEvent::Intent(intent))],
            Said::Address(value) => vec![UiEvent::Browse(BrowseEvent::Address(value))],
            Said::AddressGo => vec![UiEvent::Browse(BrowseEvent::Intent(lnomad::Intent::Go {
                url: None,
            }))],
            Said::Wheel(lines) => lnomad::gui::wheel_mice(lines)
                .into_iter()
                .map(UiEvent::Mouse)
                .collect(),
            Said::Pointer(at) => vec![UiEvent::Mouse(lnomad::gui::pointer_mouse(at))],
            // The view says nothing else: the keys, the size, the frames
            // and the transfers are the window's and the shell's here.
            Said::Key(_)
            | Said::Resized(_)
            | Said::Outcome(_)
            | Said::Frame(_)
            | Said::SystemTheme(_) => Vec::new(),
        };
        for event in events {
            self.tell(event);
        }
    }

    fn update_inner(&mut self, message: Message) -> Task<Message> {
        // The viewer is over everything: a key is its Esc or nothing.
        if let (Some(viewer), Message::Key(event)) = (&mut self.viewer, &message) {
            if leviculum_ui_iced::key::chord(event).is_some_and(|k| k.code == KeyCode::Esc)
                && viewer.back()
            {
                self.viewer = None;
            }
            return Task::none();
        }
        match message {
            // A frame is due when iced is about to draw one, so the first
            // is on the screen once the second is due.
            Message::Probed => {
                let frames = self.probe.map_or(0, |n| n + 1);
                self.probe = Some(frames);
                return match frames >= 2 {
                    true => iced::exit(),
                    false => Task::none(),
                };
            }
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
            Message::AddNearby(peer) => {
                self.screen = Screen::Chats;
                self.tell(UiEvent::AddNearby { peer });
                return Task::none();
            }
            Message::Picked { peer, photo, path } => {
                match path {
                    Ok(Some(path)) => {
                        if let Some(asks) = &self.asks {
                            let _ = asks.send(Want::Attach { peer, path, photo });
                        }
                    }
                    Ok(None) => {}
                    Err(why) => self.seen.notice = Some(why),
                }
                return Task::none();
            }
            Message::PickCode => {
                let title = code_title(self.lang);
                return Task::perform(
                    async move { desktop::pick(title, desktop::Pick::Picture).await },
                    Message::CodePicked,
                );
            }
            Message::CodePicked(path) => {
                match path.and_then(|p| p.map(|p| read_code(&p, self.lang)).transpose()) {
                    Ok(Some(code)) => {
                        self.new_address = Some(code);
                        return self.chat(Message::NewGo);
                    }
                    Ok(None) => {}
                    Err(why) => self.seen.notice = Some(why),
                }
                return Task::none();
            }
            Message::Intent(intent) => return self.intent(intent),
            Message::Browse(said) => {
                self.browse(said);
                return Task::none();
            }
            Message::Rooms(said) => {
                self.tell(UiEvent::Rooms(said));
                return Task::none();
            }
            Message::Explore(said) => {
                return self.intent(Intent::Explore(said));
            }
            // The Browse tab in front has the keys, as the model routes
            // them (1369).
            Message::Key(event) if self.screen == Screen::Browse => {
                if let Some(key) = leviculum_ui_iced::key::chord(&event) {
                    self.tell(UiEvent::Key(key));
                }
                return Task::none();
            }
            Message::TrackWindow(chosen) => {
                self.map_menu = false;
                self.tell(UiEvent::TrackWindow(chosen));
                return Task::none();
            }
            Message::MapMenu => {
                self.map_menu = !self.map_menu;
                return Task::none();
            }
            Message::TimeZones => {
                self.map_menu = false;
                self.time_zones = !self.time_zones;
                self.zone_clock = self.zone_clock.wrapping_add(1);
                let paint = self.paint();
                return match self.time_zones {
                    true => Task::batch([
                        paint,
                        after(to_next_minute(), Message::ZoneClock(self.zone_clock)),
                    ]),
                    false => paint,
                };
            }
            Message::ZoneClock(n) => {
                if !self.time_zones || n != self.zone_clock {
                    return Task::none();
                }
                let paint = self.paint();
                return Task::batch([paint, after(to_next_minute(), Message::ZoneClock(n))]);
            }
            Message::CopyText(words) => {
                self.seen.notice = Some(StrId::MailboxCopied.text(self.lang).to_string());
                return iced::clipboard::write(words);
            }
            Message::Settings => {
                self.screen = Screen::Settings;
                self.settings_at = None;
                self.menu = None;
                self.maps_asked = false;
                self.maps_usage = self.maps().usage();
                return Task::none();
            }
            Message::Key(event) => {
                if let Some(task) = self.selection_key(&event) {
                    return task;
                }
                return match self.key(&event) {
                    Some(intent) => self.intent(intent),
                    None => Task::none(),
                };
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
                self.settle();
                self.size = Some(size);
                self.hold();
                if !self.fitted {
                    self.fit();
                }
            }
            Message::Scale(scale) | Message::Window(window::Event::Rescaled(scale)) => {
                self.settle();
                self.scale = scale.max(0.25);
                self.hold();
            }
            Message::Window(window::Event::Resized(size)) => {
                self.window = size;
                leviculum_ui_iced::fps::set_size(
                    (size.width * self.scale).round() as u32,
                    (size.height * self.scale).round() as u32,
                );
                self.rescene();
                return false;
            }
            // The frame meter's size before any resize (1441).
            Message::Window(window::Event::Opened { size, .. }) => {
                leviculum_ui_iced::fps::set_size(
                    (size.width * self.scale).round() as u32,
                    (size.height * self.scale).round() as u32,
                );
                return false;
            }
            Message::Window(window::Event::Focused) => self.front(true),
            Message::Window(window::Event::Unfocused) => self.front(false),
            Message::Window(_) => return false,
            Message::Pointer(pointer) => self.pointer(pointer),
            Message::Select(peer) => {
                self.settle();
                self.selected = Some(peer);
                if let Some(pin) = self.seen.pins.iter().find(|p| p.peer == peer) {
                    self.view.centre = pin.at;
                    self.hold();
                }
            }
            Message::Painted(shown) => {
                self.busy = false;
                self.trace.painted(&shown);
                self.shown = Some(shown);
                self.prefetching = true;
            }
            Message::Frame(now) => {
                self.owed = false;
                self.tick(now);
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

    /// The map's view shown to the core's scheduler at every frame asked
    /// (1527): what it decides goes to the shell's loader, the screen at
    /// its zoom first. The platform only downloads what the core ranked.
    fn drive_loads(&mut self) {
        if !self.seen.autoload || self.screen != Screen::Map || self.size.is_none() {
            return;
        }
        let Some(asks) = &self.asks else {
            return;
        };
        let view = self.view.at_zoom(self.view.tile_zoom());
        let now = self.now_ms();
        let heading = self.motion.heading(now);
        let moving = self.moving();
        if let Some(wants) = self
            .loads
            .decide(&view, &self.setup.regions, now, moving, heading)
        {
            let _ = asks.send(Want::MapView(wants));
        }
    }

    /// The worker's next job, when it is free: the frame the view asks
    /// for unless it is on screen and owes nothing, else a prefetch batch
    /// while the frame on screen rests and the window has the focus.
    fn next_job(&mut self) -> Option<Job> {
        // Every frame the view is shown to the scheduler, busy or not.
        self.drive_loads();
        if self.busy || self.screen != Screen::Map {
            return None;
        }
        // The first job since the last pan freezes the heading the
        // prefetch plans by, before the order carries it (1189).
        self.motion.asked(self.now_ms());
        let order = self.order()?;
        let current = self.shown.as_ref().is_some_and(|s| {
            (s.order == order && s.missing == 0 && !s.fading) || self.covers(s, &order)
        });
        // While the view moves, a frame at most once in a frame interval:
        // the screen's next frame asks again.
        let now = Instant::now();
        let soon = self
            .asked_at
            .is_some_and(|at| now.saturating_duration_since(at) < FRAME);
        if !current && order.moving && soon {
            self.owed = true;
            return None;
        }
        let job = match current {
            false => {
                self.asked_at = Some(now);
                self.trace.asked(&order);
                Job::Draw(Box::new(order))
            }
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
        let (pins, radar) = self.radar_pins();
        Some(Order {
            // While the wheel's zoom glides, the view it ends on: the
            // glide scales the frame on screen towards it.
            view: self.glide.map_or(self.view, |g| g.to),
            margin: (w / MARGIN_PART, h / MARGIN_PART),
            moving: self.moving(),
            style: self.seen.style,
            scale: self.scale,
            pins,
            tracks: self.tracks(),
            radar,
            selected: self.selected,
            zooms: self.zooms(),
            epoch: self.seen.maps,
            motion: self.motion,
            time_zones: self.time_zones.then(loquax::zoneclock::unix_minute),
        })
    }

    /// The pins as the frame draws them, and the radar where no region
    /// touches the view at its tile zoom (1386): then each pin says how
    /// far and which way it is from the centre.
    fn radar_pins(&self) -> (Vec<MapPin>, Option<loquax::ui::radar::Radar>) {
        use loquax::ui::radar;
        let mut pins = self.seen.pins.clone();
        let at_rest = self.view.at_zoom(self.view.tile_zoom());
        let shown = match radar::shows(&at_rest, &self.setup.regions) {
            true => radar::radar(&pins, self.view.unit(), self.view.size(), self.lang),
            false => None,
        };
        if let Some(r) = &shown {
            radar::label(&mut pins, r);
        }
        (pins, shown)
    }

    /// The tracks at the view's tile zoom, as the frame draws them: kept
    /// while the zoom step, the window, the minute and the fixes stay, so
    /// a drag does not derive a year of fixes every frame.
    fn tracks(&self) -> Vec<loquax::ui::tracks::Track> {
        let Some(now) = self.seen.now else {
            return Vec::new();
        };
        let key = TrackKey {
            zoom: self.view.tile_zoom(),
            scale: self.scale.to_bits(),
            window: self.seen.track_window,
            minute: now.unix_secs.div_euclid(60),
            fixes: self
                .seen
                .tracks
                .iter()
                .map(|(peer, _, f)| (*peer, Arc::as_ptr(f) as usize, f.len()))
                .collect(),
        };
        let mut cache = self.track_cache.borrow_mut();
        if let Some((held, tracks)) = cache.as_ref() {
            if *held == key {
                return tracks.clone();
            }
        }
        // A screen pixel at the tile zoom, so the decimation and the
        // places' cells hold while the wheel turns within the step.
        let unit = libm::exp2(24.0 - f64::from(key.zoom));
        let tracks = loquax::ui::tracks::tracks(
            &self.seen.tracks,
            self.seen.track_window,
            &now,
            unit,
            self.scale,
            self.lang,
        );
        *cache = Some((key, tracks.clone()));
        tracks
    }

    /// Whether the window has frames to show by itself (1431): the
    /// wheel's zoom glides.
    fn animating(&self) -> bool {
        self.glide.is_some()
    }

    /// The wheel's glide at `now` (1431): the view on screen moves on,
    /// and is the view it ends on once it is over.
    fn tick(&mut self, now: Instant) {
        let Some(glide) = self.glide else {
            return;
        };
        let (view, over) = glide.at(now);
        self.view = view;
        if over {
            self.glide = None;
        }
    }

    /// The wheel's glide ended where it goes, at once: before a drag, a
    /// key or a new size moves the view from there.
    fn settle(&mut self) {
        if let Some(glide) = self.glide.take() {
            self.view = glide.to;
        }
    }

    /// Whether `shown` still serves `order` while a drag moves the view
    /// (1431): asked for the same map, at the zoom on screen, and its
    /// margins still past each edge of the screen by half their width at
    /// least. The phone pans its last frame the same way; a frame drawn
    /// for every event would cost the screen a new picture each time.
    fn covers(&self, shown: &Shown, order: &Order) -> bool {
        if !self.press.is_some_and(|p| p.dragging) || shown.missing > 0 || shown.fading {
            return false;
        }
        let same = Order {
            view: shown.order.view,
            moving: shown.order.moving,
            motion: shown.order.motion,
            ..order.clone()
        };
        if same != shown.order {
            return false;
        }
        let (x, y, k) = onto(&shown.view, &order.view);
        if (k - 1.0).abs() > 1e-9 {
            return false;
        }
        let (w, h) = shown.view.size();
        let (sw, sh) = order.view.size();
        let (mx, my) = (
            f64::from(order.margin.0) / 2.0,
            f64::from(order.margin.1) / 2.0,
        );
        -x >= mx
            && -y >= my
            && x + f64::from(w) - f64::from(sw) >= mx
            && y + f64::from(h) - f64::from(sh) >= my
    }

    /// A drag or the wheel moves the view.
    fn moving(&self) -> bool {
        self.press.is_some_and(|p| p.dragging) || self.wheel.is_some() || self.glide.is_some()
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
                async move { draw(&frames, *order) },
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

    /// "Fit all" (1386): every peer with a position inside the tracks'
    /// time window in view, as `f` does in the terminal.
    fn fit_window(&mut self) {
        let Some(size) = self.pixels() else {
            return;
        };
        let inside = match self.seen.now {
            Some(now) => {
                loquax::ui::tracks::inside(&self.seen.tracks, self.seen.track_window, &now)
            }
            None => Vec::new(),
        };
        let pins = loquax::ui::map::in_window(&self.seen.pins, &inside);
        let (centre, zoom) = fit(&pins, map::area(size), &self.setup);
        self.view = ExactView::new(centre, u32::from(zoom) * MILLI, size.0, size.1);
        self.hold();
    }

    fn ask(&self, want: MapWant) {
        if let Some(asks) = &self.asks {
            let _ = asks.send(Want::Map(want));
        }
    }

    /// A region landed or left: the regions read again, the setup with
    /// them, and the fit again when the reader has not moved since it.
    /// The tiles drop themselves on the new epoch.
    /// The maps directory the window draws from, as the shell's autoload
    /// shares it.
    fn maps(&self) -> loquax::picture::Maps {
        let frames = self.frames.lock().unwrap_or_else(PoisonError::into_inner);
        frames.painter().maps().clone()
    }

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
        self.trace.event(&pointer);
        let scale = self.scale;
        let at = |p: Point| ((p.x * scale).round() as i32, (p.y * scale).round() as i32);
        match pointer {
            Pointer::Down(p) => {
                self.settle();
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
                // The notch adds to the zoom the glide goes to, and the
                // glide starts again from the view on screen (1431): a
                // touchpad's many small turns chase their sum the same way.
                let (x, y) = at(p);
                let now = Instant::now();
                self.wheel = Some(now);
                let goal = self
                    .glide
                    .map_or(self.view.zoom_milli(), |g| g.to.zoom_milli());
                let from = self.view;
                self.zoom_to(i64::from(goal) + i64::from(by), f64::from(x), f64::from(y));
                let to = self.view;
                self.view = from;
                self.glide = (to != from).then_some(Glide {
                    from,
                    to,
                    start: now,
                });
            }
        }
    }

    /// The window, and the frame meter's numbers over it while it
    /// measures (`--fps`, 1441).
    fn view(&self) -> Element<'_, Message> {
        // The event that set it is over.
        self.drop_open.set(false);
        let window = self.window_view();
        match leviculum_ui_iced::fps::enabled() {
            true => stack![window, leviculum_ui_iced::fps::overlay()].into(),
            false => window,
        }
    }

    fn window_view(&self) -> Element<'_, Message> {
        if let Some(viewer) = &self.viewer {
            return viewer.view();
        }
        let body = match (self.screen, self.class(), &self.scene.chat) {
            (Screen::Map, _, _) => self.map_view(),
            (Screen::Browse, _, _) => self.browse_view(),
            (Screen::Rooms, _, _) => self.rooms_view(),
            (Screen::Explore, _, _) => self.explore_pane(),
            (Screen::Settings, _, _) => self.settings_pane(),
            // A group's screens (1307) take the chat's place, or the
            // whole stack's on a narrow window.
            (Screen::Chats, LayoutClass::Wide, _) if self.group_screen() => {
                row![self.list_pane(), self.group_pane()].into()
            }
            (Screen::Chats, LayoutClass::Compact, _) if self.group_screen() => self.group_pane(),
            (Screen::Chats, LayoutClass::Wide, _) => {
                row![self.list_pane(), self.chat_pane()].into()
            }
            (Screen::Chats, LayoutClass::Compact, Some(_)) => self.chat_pane(),
            (Screen::Chats, LayoutClass::Compact, None) => self.list_pane(),
        };
        let window: Element<'_, Message> = match (self.tabs(), self.class()) {
            (Some(tabs), LayoutClass::Wide) => row![tabs, body].into(),
            (Some(tabs), LayoutClass::Compact) => column![body, tabs].into(),
            (None, _) => body,
        };
        match (self.help, &self.scene.profile) {
            (Some(scroll), _) => stack![window, self.help_layer(scroll)].into(),
            (None, Some(profile)) => stack![window, self.profile_layer(profile)].into(),
            (None, None) => window,
        }
    }

    /// The profile over the window (1443): the scene's
    /// [`loquax::ui::profile::ProfileScreen`] as the phone's activity
    /// draws it, the avatar, the name, the address in fours, the mailbox
    /// and the devices, the code's QR on white, "Share my address" and
    /// Close.
    fn profile_layer<'a>(
        &'a self,
        profile: &'a loquax::ui::profile::ProfileScreen,
    ) -> Element<'a, Message> {
        let t = self.tokens();
        let lang = self.lang;
        // The address and the code select (1475), as runs of their own.
        let picked = self.picked_ranges();
        let selects = |words: String, part: loquax::ui::selection::Part, size: f32| {
            let key = loquax::ui::selection::RunKey::new(0, part);
            let ink = chat::colour(t.text);
            leviculum_ui_iced::select::selectable(
                text(words.clone())
                    .size(size)
                    .font(iced::Font::MONOSPACE)
                    .align_x(iced::Alignment::Center)
                    .width(Length::Fill)
                    .wrapping(iced::widget::text::Wrapping::WordOrGlyph)
                    .color(ink),
                words,
                picked.get(&key).cloned(),
                Color { a: 0.3, ..ink },
                self.picking,
                move |pick| Message::PickText(key, pick),
            )
        };
        let said = |words: &loquax::ui::strings::Text, size: f32, ink: u32| {
            text(words.resolve(lang))
                .size(size)
                .align_x(iced::Alignment::Center)
                .width(Length::Fill)
                .color(chat::colour(ink))
        };
        let mut words = column![
            said(&profile.title, 18.0, t.text),
            chat::avatar_of(&profile.avatar, 64.0, t),
            said(&profile.name, 20.0, t.text),
            said(&profile.address_label, 13.0, t.text_secondary),
            selects(
                profile.fours.clone(),
                loquax::ui::selection::Part::Address,
                13.0
            ),
            said(&profile.mailbox, 14.0, t.text),
        ]
        .spacing(6)
        .align_x(iced::Alignment::Center);
        if let Some(state) = &profile.state {
            words = words.push(said(state, 13.0, t.text_secondary));
        }
        if !profile.devices.is_empty() {
            words = words.push(said(&profile.devices_label, 13.0, t.text_secondary));
            for device in &profile.devices {
                words = words.push(said(device, 14.0, t.text));
            }
        }
        let button_of = |action: &loquax::ui::scene::Action| {
            let label = text(action.label_key.text(lang))
                .size(15)
                .color(chat::colour(match action.enabled {
                    true => t.accent,
                    false => t.text_secondary,
                }));
            button(label)
                .on_press_maybe(
                    action
                        .enabled
                        .then(|| Message::Intent(action.intent.clone())),
                )
                .style(|_: &Theme, _| button::Style::default())
        };
        words = words.push(row![button_of(&profile.share), button_of(&profile.back)].spacing(12));
        // The code beside the words, as wide as a phone's is tall.
        let mut body = row![words.width(Length::Fixed(340.0))]
            .spacing(20)
            .align_y(iced::Alignment::Center);
        if let Some(qr) = &profile.qr {
            body = body.push(
                column![
                    Canvas::new(QrCanvas {
                        size: qr.side,
                        modules: qr.modules.clone(),
                    })
                    .width(QR_SIDE)
                    .height(QR_SIDE),
                    said(&profile.caption, 13.0, t.text_secondary),
                ]
                // The code the QR holds, as words to select and copy (1475).
                .extend(
                    profile
                        .code
                        .clone()
                        .map(|code| selects(code, loquax::ui::selection::Part::Code, 11.0).into()),
                )
                .spacing(6)
                .width(QR_SIDE),
            );
        }
        let panel = container(iced::widget::scrollable(body).height(Length::Shrink))
            .padding(20)
            .style(move |_: &Theme| container::Style {
                background: Some(iced::Background::Color(chat::colour(t.panel))),
                border: iced::border::rounded(12),
                ..container::Style::default()
            });
        container(panel).center(Length::Fill).into()
    }

    /// The window in the help's cells.
    fn help_area(&self) -> leviculum_ui::Rect {
        let cells = |px: f32, cell: f32| (px / cell).clamp(0.0, f32::from(u16::MAX)) as u16;
        leviculum_ui::Rect::new(
            0,
            0,
            cells(self.window.width, lnomad::gui::CELL_W),
            cells(self.window.height, lnomad::gui::CELL_H),
        )
    }

    /// The keys' help over the window (1371): the keymap's rows in the
    /// shared overlay, on the cell grid lnomad's window lays its help on;
    /// a click closes it.
    fn help_layer(&self, scroll: usize) -> Element<'_, Message> {
        let grid = leviculum_ui_iced::help::Grid {
            regular: lnomad::gui::MONO,
            bold: lnomad::gui::MONO_BOLD,
            size: lnomad::gui::PAGE_TEXT,
            cell_w: lnomad::gui::CELL_W,
            cell_h: lnomad::gui::CELL_H,
        };
        let overlay = leviculum_ui_iced::help::overlay(
            &loquax::ui::help_groups(),
            StrId::Help.text(self.lang),
            scroll,
            self.help_area(),
            grid,
            chat::palette(self.tokens()),
        );
        iced::widget::mouse_area(container(overlay).center(Length::Fill))
            .on_press(Message::Intent(Intent::Dismiss))
            .into()
    }

    /// The Browse tab (1369): lnomad's window view of the browser the
    /// model hosts, with the pictures the shell's half of lnomad's picture
    /// code holds. No second renderer: the page, its bars, its places
    /// sheet and its help are lnomad's.
    fn browse_view(&self) -> Element<'_, Message> {
        let Some(browsed) = &self.seen.browse else {
            return Space::new().into();
        };
        let pictures = browsed
            .pictures
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        lnomad::gui::view(&browsed.model, &pictures).map(Message::Browse)
    }

    /// The Rooms tab (1379), a room as a chat since 1450: drawn by
    /// [`App::rooms_pane`] from the scene's Rooms node.
    fn rooms_view(&self) -> Element<'_, Message> {
        self.rooms_pane()
    }

    fn map_view(&self) -> Element<'_, Message> {
        let list = self
            .seen
            .pins
            .iter()
            .fold(column![].spacing(2), |list, pin| {
                let alpha = if pin.stale { 0.5 } else { 1.0 };
                let age = pin.age.clone().unwrap_or_default();
                let mut entry = column![text(pin.label.clone()).size(15), text(age).size(12),];
                // A node nearby that is no contact (1302): its card says
                // so and offers to add it.
                if pin.nearby {
                    entry = entry
                        .push(text(StrId::NearbyCard.text(Lang::En)).size(12))
                        .push(
                            button(text(StrId::NearbyAdd.text(Lang::En)).size(12))
                                .on_press(Message::AddNearby(pin.peer))
                                .padding([0, 6]),
                        );
                }
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
            true => Space::new().into(),
            false => chat::soft(scrollable(list)),
        };
        let credit = self
            .shown
            .as_ref()
            .map_or(String::new(), |s| s.attribution.clone());
        // The bar's words on one line each and the bar of one height
        // (1441): a notice or a credit too long for it wrapped to a second
        // line, which took the map 17 pixels of height while a pan loaded
        // its tiles, and the map's frame was drawn anew at the new size.
        let line = |words: String| text(words).size(13).wrapping(text::Wrapping::None);
        let bar = row![
            button(text("Chats").size(13))
                .on_press(Message::Intent(Intent::Tab(TabId::Chats)))
                .padding([0, 6])
                .style(button::text),
            // "Map", or "Map · This week" when the tracks' window is not
            // the default (1419): why older lines show.
            line(self.seen.track_window.title(self.lang)),
            line("  ".to_string()),
            // Always on screen: the region's credit, or the outline's.
            line(credit),
            container(line(
                [self.seen.loading.clone(), self.seen.notice.clone()]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" · ")
            ))
            .width(Length::Fill)
            .align_right(Length::Fill)
            .clip(true),
            // The zoom is a debug overlay's, with the frame counter (1447).
            line(match leviculum_ui_iced::fps::enabled() {
                true => format!("  z{:.1}", self.view.zoom()),
                false => String::new(),
            }),
            // The map's menu (1419): the time window, rarely needed, is
            // in it and not on the map.
            button(text(StrId::Menu.text(self.lang)).size(13))
                .on_press(Message::MapMenu)
                .padding([0, 6])
                .style(match self.map_menu {
                    true => button::primary,
                    false => button::text,
                }),
        ]
        .padding([2, 8])
        .height(MAP_BAR);
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
        let menu: Element<'_, Message> = match self.map_menu {
            true => self.map_menu(),
            false => Space::new().into(),
        };
        // Nobody else's position (1447, walk 1 finding 18): one sentence
        // in the middle of the map, which shows the own area where there
        // is an own position and the theme's ground where there is none.
        let empty: Element<'_, Message> = match self.seen.pins.iter().all(|p| p.own) {
            true => container(
                text(StrId::MapEmpty.text(self.lang))
                    .size(16)
                    .color(chat::colour(self.tokens().text_secondary)),
            )
            .center(Length::Fill)
            .into(),
            false => Space::new().into(),
        };
        let map = container(
            stack![picture, canvas, empty, menu]
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

impl App {
    /// The map's menu over its bottom right corner (1419): the tracks'
    /// time window as a single choice, the default first and the one
    /// shown marked, and "fit all" (1386), every peer inside the window
    /// in view. A choice closes it.
    fn map_menu(&self) -> Element<'_, Message> {
        use loquax::ui::tracks::TrackWindow;
        let t = self.tokens();
        let heading = text(StrId::TrackWindow.text(self.lang))
            .size(13)
            .color(chat::colour(t.text_secondary));
        let choices = TrackWindow::CHOICES
            .iter()
            .fold(column![heading].spacing(2), |list, &w| {
                let style = match self.seen.track_window == w {
                    true => button::primary,
                    false => button::text,
                };
                list.push(
                    button(text(w.label().text(self.lang)).size(13))
                        .on_press(Message::TrackWindow(w))
                        .padding([2, 8])
                        .width(Length::Fill)
                        .style(style),
                )
            })
            .push(
                button(text(StrId::FitWindow.text(self.lang)).size(13))
                    .on_press(Message::Intent(Intent::Map(MapCmd::FitWindow)))
                    .padding([2, 8])
                    .width(Length::Fill)
                    .style(button::text),
            )
            // The time zone view (1684): one entry that turns it on and
            // off, marked while it is on.
            .push(
                button(text(StrId::TimeZones.text(self.lang)).size(13))
                    .on_press(Message::TimeZones)
                    .padding([2, 8])
                    .width(Length::Fill)
                    .style(match self.time_zones {
                        true => button::primary,
                        false => button::text,
                    }),
            )
            .width(Length::Fixed(200.0));
        // The background's ground with a line round it, so the menu
        // stands off the map under it.
        let panel = container(choices)
            .padding(8)
            .style(move |_: &Theme| container::Style {
                background: Some(iced::Background::Color(chat::colour(t.background))),
                border: iced::border::rounded(8)
                    .color(chat::colour(t.dim))
                    .width(1.0),
                ..container::Style::default()
            });
        container(panel)
            .padding(8)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_right(Length::Fill)
            .align_bottom(Length::Fill)
            .into()
    }
}

/// The frame on screen, where the view moved it.
struct PictureCanvas<'a> {
    app: &'a App,
}

/// The side of "My code"'s QR, in logical pixels.
const QR_SIDE: f32 = 260.0;

/// A QR code's modules drawn dark on white with a quiet zone of four.
struct QrCanvas {
    size: u32,
    modules: Vec<bool>,
}

impl canvas::Program<Message> for QrCanvas {
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
        frame.fill_rectangle(Point::ORIGIN, bounds.size(), Color::WHITE);
        let size = self.size as usize;
        let cell = bounds.width.min(bounds.height) / (size + 8) as f32;
        for (i, dark) in self.modules.iter().enumerate() {
            if *dark {
                let (x, y) = ((i % size + 4) as f32, (i / size + 4) as f32);
                frame.fill_rectangle(
                    Point::new(x * cell, y * cell),
                    Size::new(cell, cell),
                    Color::BLACK,
                );
            }
        }
        vec![frame.into_geometry()]
    }
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
        let whole = Rectangle::with_size(bounds.size());
        let mut covered = None;
        if let Some((shown, x, y, k)) = self.app.onto() {
            let scale = f64::from(self.app.scale);
            let (w, h) = shown.view.size();
            // Scaled smoothly only at rest: while the wheel's zoom glides
            // the nearest pixel costs the software renderer half of the
            // smooth scaling (13 against 22 ms a frame at 1860 x 1024,
            // 1431), and the glide ends on a frame drawn at its zoom.
            let filter = match (k - 1.0).abs() < 1e-9 || self.app.moving() {
                true => FilterMethod::Nearest,
                false => FilterMethod::Linear,
            };
            let at = Rectangle {
                x: (x / scale) as f32,
                y: (y / scale) as f32,
                width: (f64::from(w) * k / scale) as f32,
                height: (f64::from(h) * k / scale) as f32,
            };
            covered = at.intersection(&whole);
            frame.draw_image(
                at,
                canvas::Image::new(shown.handle.clone())
                    .filter_method(filter)
                    .snap(true),
            );
        }
        // Never black (1441): what no frame covers yet, past a drag's
        // margin, round a frame the wheel's zoom shrinks, before the first
        // frame, is the map's ground in its theme, the land a frame is
        // cleared to, and not the window's background under the canvas.
        // Only the strips the frame leaves, a pixel into it against the
        // snapped edge, so the ground costs no second pass under the map.
        let [r, g, b] =
            leviculum_mapfill::frame::rgb888(self.app.seen.style.theme.palette().background);
        for strip in uncovered(whole, covered) {
            frame.fill_rectangle(strip.position(), strip.size(), Color::from_rgb8(r, g, b));
        }
        vec![frame.into_geometry()]
    }
}

/// The parts of `whole` outside `covered`, each reaching a pixel into it:
/// above, below, left and right of it; `whole` itself when nothing covers.
fn uncovered(whole: Rectangle, covered: Option<Rectangle>) -> Vec<Rectangle> {
    let Some(c) = covered else {
        return vec![whole];
    };
    let (top, bottom) = (c.y + 1.0, c.y + c.height - 1.0);
    let (left, right) = (c.x + 1.0, c.x + c.width - 1.0);
    [
        Rectangle::new(Point::ORIGIN, Size::new(whole.width, top)),
        Rectangle::new(
            Point::new(0.0, bottom),
            Size::new(whole.width, whole.height - bottom),
        ),
        Rectangle::new(Point::new(0.0, c.y), Size::new(left, c.height)),
        Rectangle::new(
            Point::new(right, c.y),
            Size::new(whole.width - right, c.height),
        ),
    ]
    .into_iter()
    .filter_map(|r| r.intersection(&whole))
    .filter(|r| r.width >= 1.0 && r.height >= 1.0)
    .collect()
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
            // A peer with a track in its hue, the name too (1385).
            let hue = map::pin_hue(&shown.order.tracks, &pin.peer, false)
                .filter(|_| !pin.nearby)
                .map(|[r, g, b]| Color::from_rgb8(r, g, b));
            let blue = hue.unwrap_or(Color::from_rgb8(0x20, 0x50, 0xc0));
            let [or, og, ob] = map::OWN_DOT;
            let ink = match (pin.selected, pin.nearby, pin.stale) {
                (true, ..) => Color::from_rgb8(0xd0, 0x30, 0x30),
                // Our own position, the blue dot (1449 item 2).
                (false, _, stale) if pin.own => {
                    Color::from_rgb8(or, og, ob).scale_alpha(if stale { 0.45 } else { 1.0 })
                }
                // A node nearby that is no contact (1302): a neutral grey.
                (false, true, false) => Color::from_rgb8(0x80, 0x80, 0x80),
                (false, true, true) => Color::from_rgba8(0x80, 0x80, 0x80, 0.45),
                (false, false, false) => blue,
                (false, false, true) => blue.scale_alpha(0.45),
            };
            let centre = to(pin.x, pin.y);
            let radius = map::MARKER / 2.0;
            frame.fill(&Path::circle(centre, radius + 1.5), Color::WHITE);
            frame.fill(&Path::circle(centre, radius), ink);
            // A battery below a fifth (1386), as the phone's frame draws it.
            if pin.low_battery {
                let (bw, bh) = map::BATTERY;
                let (x, y) = map::battery_at(centre.x, centre.y, 1.0);
                let at = Point::new(x, y);
                frame.fill_rectangle(
                    Point::new(at.x - 1.0, at.y - 1.0),
                    Size::new(bw + 3.5, bh + 2.0),
                    Color::from_rgba8(0xff, 0xff, 0xff, 0.85),
                );
                frame.fill_rectangle(at, Size::new(bw, bh), Color::from_rgb8(0x10, 0x10, 0x10));
                frame.fill_rectangle(
                    Point::new(at.x + 1.0, at.y + 1.0),
                    Size::new(bw - 2.0, bh - 2.0),
                    Color::WHITE,
                );
                frame.fill_rectangle(
                    Point::new(at.x + 1.0, at.y + 1.0),
                    Size::new(((bw - 2.0) / 5.0).max(1.0), bh - 2.0),
                    Color::from_rgb8(0xd0, 0x30, 0x30),
                );
                frame.fill_rectangle(
                    Point::new(at.x + bw, at.y + bh / 3.0),
                    Size::new(1.5, bh / 3.0),
                    Color::from_rgb8(0x10, 0x10, 0x10),
                );
            }
            if let Some((x, y, words)) = &pin.label {
                let text_ink = match (pin.stale, hue) {
                    (true, Some(hue)) => hue.scale_alpha(0.55),
                    (false, Some(hue)) => hue,
                    (true, None) => Color::from_rgba8(0x20, 0x20, 0x20, 0.55),
                    (false, None) => Color::from_rgb8(0x10, 0x10, 0x10),
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

/// What the held microphone follows (1276): the pointer's moves and the
/// left button's release, anywhere in the window.
fn held_pointer(
    event: iced::Event,
    _status: iced::event::Status,
    _: window::Id,
) -> Option<Message> {
    match event {
        iced::Event::Mouse(mouse::Event::CursorMoved { position }) => {
            Some(Message::RecCursor(position))
        }
        iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
            Some(Message::RecRelease)
        }
        _ => None,
    }
}

/// The chooser's title for a picture of a code, in the window's words.
pub(crate) fn code_title(lang: Lang) -> &'static str {
    StrId::CodeFromPicture.text(lang)
}

/// The first QR code in the picture at `path` (1341), the phone's
/// scanner's decoder over the file's pixels; a picture without one is
/// said in the reader's words, without the file's path (1377).
fn read_code(path: &std::path::Path, lang: Lang) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    loquax::scan::image_codes(&bytes)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .into_iter()
        .next()
        .ok_or_else(|| StrId::CodeNotInPicture.text(lang).to_string())
}

/// The language the desktop's locale names: the scene's words are said in
/// it.
fn locale() -> Lang {
    loquax::engine::locale()
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_map::mercator::{project, LatLon};
    use map::tests::{florence, pin};

    /// The window over the Florence fixture, 480 x 320 at a density of
    /// one, `pins` seen, the view at `zoom_milli` over Piazza d'Azeglio.
    pub(super) fn window_over_florence(pins: Vec<MapPin>, zoom_milli: u32) -> App {
        let frames = florence();
        let maps = frames.painter().epoch();
        let mut app = App::new(frames, None);
        app.screen = Screen::Map;
        app.apply(Message::MapSize(Size::new(480.0, 320.0)));
        app.apply(Message::Seen(Box::new(Seen {
            pins,
            tracks: Vec::new(),
            track_window: Default::default(),
            notice: None,
            loading: None,
            maps,
            autoload: false,
            style: Style::SCREEN,
            scene: crate::shell::Model::new(&[]).scene(LayoutClass::Wide),
            now: None,
            receipts: true,
            photo_size: loquax::ui::photos::PhotoSize::default(),
            mailbox: None,
            photo: None,
            browse: None,
            copied: None,
            hubs: Vec::new(),
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
                    let shown = draw(&app.frames, *order);
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

    /// The scheduler's decisions the window sent the shell.
    fn decisions(rx: &mut tokio::sync::mpsc::UnboundedReceiver<Want>) -> Vec<ViewWants> {
        let mut out = Vec::new();
        while let Ok(want) = rx.try_recv() {
            if let Want::MapView(wants) = want {
                out.push(wants);
            }
        }
        out
    }

    /// A pan into the empty asks the loader (1527): the window shows its
    /// map's frames to the core's scheduler as the phone does, and a drag
    /// four screens east of the Florence region sends the shell the
    /// screen's own box at its zoom first, holding the screen, then the
    /// cells. The control: with the autoload off the same drag sends
    /// nothing.
    #[test]
    fn a_pan_into_the_empty_asks_the_loader() {
        for autoload in [true, false] {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let mut app = window_over_florence(Vec::new(), 16 * MILLI);
            app.asks = Some(tx);
            app.seen.autoload = autoload;
            run(&mut app);
            // The start is the fixture's edge: an eighth of it uncovered.
            let held = decisions(&mut rx);
            assert_eq!(held.len(), usize::from(autoload), "{held:?}");
            app.apply(Message::Pointer(Pointer::Down(Point::new(400.0, 150.0))));
            for x in (0..40).rev() {
                app.apply(Message::Pointer(Pointer::Move(Point::new(
                    x as f32 * 10.0,
                    150.0,
                ))));
            }
            for _ in 0..4 {
                app.apply(Message::Pointer(Pointer::Up));
                app.apply(Message::Pointer(Pointer::Down(Point::new(400.0, 150.0))));
                app.apply(Message::Pointer(Pointer::Move(Point::new(0.0, 150.0))));
            }
            app.apply(Message::Pointer(Pointer::Up));
            run(&mut app);
            let sent = decisions(&mut rx);
            if !autoload {
                assert!(held.is_empty() && sent.is_empty(), "{sent:?}");
                continue;
            }
            let last = sent.last().expect("a decision at rest");
            assert_eq!(last.share, 1.0, "in the empty");
            let first = &last.here[0];
            assert_eq!(
                first.reason,
                loquax::ui::map::LoadReason::Screen,
                "{last:?}"
            );
            assert_eq!(first.zooms, (14, 15));
            let screen = app.view.at_zoom(app.view.tile_zoom()).bounds();
            assert!(first.bounds.holds(&screen), "{:?} {screen:?}", first.bounds);
            assert!(last.here[1..]
                .iter()
                .all(|w| w.reason == loquax::ui::map::LoadReason::View));
        }
    }

    use super::first_tile;

    /// The window's first-tile walk (1527): the map at Lew's phone size
    /// (1527 x 2099, a density of one) over the start region of
    /// [`first_tile::start_region`], the synthetic Grunewald archive
    /// served throttled, the shell's map half (the loader) run here. The
    /// window draws no frame: what it would draw is read from the disk,
    /// the next frame over the screen draws a tile that is there.
    struct WindowWalk {
        _serve: leviculum_mapcut::serve::Serve,
        _dir: tempfile::TempDir,
        app: App,
        rx: tokio::sync::mpsc::UnboundedReceiver<Want>,
        loader: loquax::mapload::Loader,
        maps: loquax::picture::Maps,
        start: Instant,
        start_region: leviculum_map::region::Header,
        uncovered_ms: Option<u64>,
        loading_ms: Option<u64>,
        on_disk_ms: Option<u64>,
        cell_ms: Option<u64>,
        /// The loader's steps on the walk's clock (1616): the order the
        /// bar is read from.
        steps: Vec<first_tile::Stepped>,
    }

    const WALK_SIZE: (u32, u32) = (1527, 2099);

    impl WindowWalk {
        fn new() -> Self {
            use leviculum_mapcut::serve::Serve;
            let serve = Serve::start(vec![("p.pmtiles".into(), first_tile::first_tile_archive())])
                .expect("serve");
            let dir = tempfile::tempdir().expect("home");
            let path = dir.path().join("maps");
            std::fs::create_dir_all(&path).expect("maps");
            let at = project(LatLon::from_degrees(
                first_tile::FIRST_TILE_AT.0,
                first_tile::FIRST_TILE_AT.1,
            ));
            let url = serve.url("p.pmtiles");
            let start_region = first_tile::start_region(&path, &url, at, WALK_SIZE);
            serve.throttle(first_tile::FIRST_TILE_RATE, Duration::from_millis(20));
            let maps = loquax::picture::Maps::new(Some(path));
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            let mut app = App::new(Frames::new(maps.clone()), Some(tx));
            app.screen = Screen::Map;
            let (w, h) = WALK_SIZE;
            app.apply(Message::MapSize(Size::new(w as f32, h as f32)));
            let mut seen = app.seen.clone();
            seen.autoload = true;
            seen.maps = maps.epoch();
            app.apply(Message::Seen(Box::new(seen)));
            app.view = ExactView::new(at, 16 * MILLI, w, h);
            app.hold();
            // The walk's view is the reader's: a region landing fits
            // nothing again.
            app.fitted = true;
            // No frame is drawn: the walk reads the disk.
            app.busy = true;
            let loader = loquax::mapload::Loader::new(
                maps.clone(),
                loquax::mapload::Source::Url(url),
                1 << 30,
            );
            let mut walk = Self {
                _serve: serve,
                _dir: dir,
                app,
                rx,
                loader,
                maps,
                start: Instant::now(),
                start_region,
                uncovered_ms: None,
                loading_ms: None,
                on_disk_ms: None,
                cell_ms: None,
                steps: Vec::new(),
            };
            walk.step();
            assert!(!walk.loader.loading_here(), "the start is held");
            walk
        }

        fn now_ms(&self) -> u64 {
            self.start.elapsed().as_millis() as u64
        }

        /// One frame interval: the window asks its frame (and shows the
        /// view to the scheduler), the shell's half takes what it sent and
        /// polls the loads, a region landed is seen, the disk is read.
        fn step(&mut self) {
            let t = self.now_ms();
            self.app.tick(Instant::now());
            self.app.next_job();
            let mut steps = Vec::new();
            while let Ok(want) = self.rx.try_recv() {
                match want {
                    Want::MapView(w) => {
                        steps.extend(self.loader.schedule(w.here, w.ahead, t));
                    }
                    Want::Map(w) => {
                        steps.extend(self.loader.ask(w, t));
                    }
                    _ => {}
                }
            }
            let polled = self.loader.poll(t);
            self.steps.extend(
                steps
                    .iter()
                    .chain(&polled)
                    .filter(|s| s.kind() != loquax::mapload::StepKind::Skipped)
                    .map(|s| (t, s.kind(), s.want().clone())),
            );
            if polled
                .iter()
                .any(|s| matches!(s, loquax::mapload::Step::Done { .. }))
            {
                let mut seen = self.app.seen.clone();
                seen.maps = self.maps.epoch();
                self.app.apply(Message::Seen(Box::new(seen)));
            }
            self.look(t);
            let next = t + 16;
            let now = self.now_ms();
            if next > now {
                std::thread::sleep(Duration::from_millis(next - now));
            }
        }

        fn look(&mut self, t: u64) {
            let view = self.app.view.at_zoom(self.app.view.tile_zoom());
            let maps = self.maps.dir().expect("dir").to_path_buf();
            if self.uncovered_ms.is_none()
                && loquax::ui::viewload::uncovered(&view, &[self.start_region]) > 0.10
            {
                self.uncovered_ms = Some(t);
            }
            if self.uncovered_ms.is_some()
                && self.loading_ms.is_none()
                && self.loader.loading_here()
            {
                self.loading_ms = Some(t);
            }
            let start = ["start.lmr".to_string()];
            if self.on_disk_ms.is_none()
                && first_tile::arrived(&maps, &start, &view, view.zoom().min(15))
            {
                self.on_disk_ms = Some(t);
            }
            if self.cell_ms.is_none()
                && first_tile::region_files(&maps)
                    .iter()
                    .any(|(name, z, _)| z.1 - z.0 >= 2 && name != "start.lmr")
            {
                self.cell_ms = Some(t);
            }
        }

        /// A drag from `from` by `dx` a frame for `frames` frames, the
        /// pointer held down.
        fn drag(&mut self, from: f32, dx: f32, frames: u32) {
            let y = WALK_SIZE.1 as f32 / 2.0;
            self.app
                .apply(Message::Pointer(Pointer::Down(Point::new(from, y))));
            for k in 1..=frames {
                self.app.apply(Message::Pointer(Pointer::Move(Point::new(
                    from + dx * k as f32,
                    y,
                ))));
                self.step();
            }
        }

        /// The pointer still down at `at`, moved on by `dx` a frame.
        fn drag_on(&mut self, at: f32, dx: f32, frames: u32) {
            let y = WALK_SIZE.1 as f32 / 2.0;
            for k in 1..=frames {
                self.app.apply(Message::Pointer(Pointer::Move(Point::new(
                    at + dx * k as f32,
                    y,
                ))));
                self.step();
            }
        }

        fn release(&mut self) {
            self.app.apply(Message::Pointer(Pointer::Up));
        }

        /// At rest until a tile of the screen is there and, when asked
        /// for, a cell, at most `limit_ms`.
        fn rest(&mut self, limit_ms: u64, cell: bool) {
            let until = self.now_ms() + limit_ms;
            while self.now_ms() < until
                && (self.on_disk_ms.is_none() || cell && self.cell_ms.is_none())
            {
                self.step();
            }
        }

        fn row(&self, name: &str) -> String {
            let after = |t: Option<u64>| match (self.uncovered_ms, t) {
                (Some(u), Some(t)) => format!("{}", t as i64 - u as i64),
                _ => "never".to_string(),
            };
            format!(
                "| {name} | {} | {} | {} | {} |",
                self.uncovered_ms
                    .map_or("never".to_string(), |u| u.to_string()),
                after(self.loading_ms),
                after(self.on_disk_ms),
                after(self.cell_ms),
            )
        }
    }

    /// The screen at its zoom loads first in the window too (1527): the
    /// gestures of the phone's walk the window has (no coast on a desk;
    /// a drag released at speed stops), from the frame that first leaves a
    /// tenth of the screen uncovered by the start region: when a load for
    /// the screen was said, when a tile of the screen at the zoom viewed
    /// was on disk (`first_tile_ms`: the next frame draws it), and when a
    /// cell's first byte showed. The bar is the loader's order (1616,
    /// [`first_tile::out_of_order`]): a tile of the screen arrives, and a
    /// cell started after the uncovered frame starts only once no box of
    /// the screen runs and every one asked since landed. The disk times
    /// are reported, not asserted: a starved host writes the file of a
    /// cell the pan's ahead started before the uncovered frame after it.
    #[test]
    fn the_screen_at_its_zoom_draws_first_in_the_window() {
        let mut rows = Vec::new();
        let mut late = Vec::new();
        let blob = first_tile::first_tile_blob();
        let mut check = |walk: &WindowWalk, name: &str| {
            rows.push(walk.row(name));
            // The archive's bytes of each load: its tiles, each a blob of
            // the tile and its pad.
            let maps = walk.maps.dir().expect("dir").to_path_buf();
            for (file, zooms, _) in first_tile::region_files(&maps) {
                if file == "start.lmr" {
                    continue;
                }
                let opened = std::fs::File::open(maps.join(&file)).expect("open");
                let tiles = leviculum_map::region::Region::open(opened)
                    .expect("region")
                    .header()
                    .tiles;
                eprintln!(
                    "{name}: load z{}-{}: {tiles} tiles, {} archive bytes",
                    zooms.0,
                    zooms.1,
                    u64::from(tiles) * blob
                );
            }
            let Some(u) = walk.uncovered_ms else {
                late.push(format!("{name}: the screen was never uncovered"));
                return;
            };
            first_tile::print_steps(name, &walk.steps, u);
            if walk.on_disk_ms.is_none() {
                late.push(format!("{name}: no tile of the screen at its zoom arrived"));
            }
            late.extend(
                first_tile::out_of_order(&walk.steps, u)
                    .into_iter()
                    .map(|why| format!("{name}: {why}")),
            );
        };
        let w = WALK_SIZE.0 as f32;
        // A drag of a screen and a half east (the view goes east, the
        // pointer west) in 36 frames, then at rest.
        let mut walk = WindowWalk::new();
        walk.drag(w - 50.0, -(w * 1.5) / 36.0, 36);
        walk.release();
        walk.rest(15_000, true);
        check(&walk, "pan, then rest");
        // The same drag, then on slowly for two seconds, held.
        let mut walk = WindowWalk::new();
        walk.drag(w - 50.0, -(w * 1.5) / 36.0, 36);
        let at = w - 50.0 - w * 1.5;
        walk.drag_on(at, -2.0, 125);
        walk.release();
        walk.rest(15_000, true);
        check(&walk, "pan, on slowly");
        // Two notches of the wheel out about the middle in eight frames.
        let mut walk = WindowWalk::new();
        let middle = Point::new(w / 2.0, WALK_SIZE.1 as f32 / 2.0);
        for _ in 0..8 {
            walk.app
                .apply(Message::Pointer(Pointer::Wheel(middle, -250)));
            walk.step();
        }
        for _ in 0..30 {
            walk.step();
        }
        walk.app.apply(Message::WheelRest);
        walk.rest(15_000, true);
        check(&walk, "zoom out two steps");
        eprintln!("| gesture | uncovered at ms | loading after ms | first_tile_ms (on disk) | cell after ms |");
        eprintln!("|---|---|---|---|---|");
        for row in &rows {
            eprintln!("{row}");
        }
        assert!(
            late.is_empty(),
            "a cell before the screen's box, or no tile of the screen: {late:?}"
        );
    }

    /// A fractional wheel zoom stays where it ends (1144): a touchpad's
    /// 0.37 of a step about a point keeps the world point there and the
    /// zoom between the steps. It glides there (1431): right after the
    /// turn the view on screen is still the old one, the frame asked is a
    /// moving one at the zoom the glide ends on, drawn at that exact zoom
    /// (not at the step, so the glide ends on it sharp), and the frame on
    /// screen scaled onto the view keeps the point under the pointer. At
    /// the glide's end the view is that zoom; once the wheel rests the
    /// frame at rest is drawn at it, with its names, and nothing asks for
    /// another: the next job is the prefetch.
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
        assert_eq!(app.view.zoom_milli(), 14_000, "the glide has not moved yet");
        let (turning, _) = run(&mut app);
        let turning = turning.expect("a frame while the wheel turns");
        assert!(turning.order.moving);
        assert_eq!(
            turning.view.zoom_milli(),
            14_370,
            "drawn at the glide's end"
        );
        let (tx, ty, k) = onto(&turning.view, &app.view);
        assert!((k - 2f64.powf(-0.37)).abs() < 1e-9, "{k}");
        let (fx, fy) = turning.view.to_screen(held);
        let (x, y) = (tx + k * fx, ty + k * fy);
        assert!(
            (x - under.0).abs() < 1.0 && (y - under.1).abs() < 1.0,
            "{x} {y}"
        );
        app.tick(Instant::now() + Duration::from_millis(GLIDE_MS));
        assert!(!app.animating(), "the glide is over");
        assert_eq!(app.view.zoom_milli(), 14_370);
        let (_, _, k) = onto(&turning.view, &app.view);
        assert!((k - 1.0).abs() < 1e-9, "it ends on the frame drawn for it");
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

    use leviculum_ui_iced::select::Pick;
    use loquax::outbox::OutboxEvent;
    use loquax::ui::selection::{Part, RunKey};
    use loquax::ui::update;

    pub(super) const ANNA: [u8; 16] = [0x3a; 16];
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
                sent_at: 0,
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
    pub(super) fn chat_model() -> crate::shell::Model {
        let mut model = crate::shell::Model::new(&[]);
        // The window keeps its scenes whole, as `run` sets it (1490).
        update(&mut model, UiEvent::Viewing(loquax::ui::Viewing::ALL));
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

    pub(super) fn seen_of(model: &crate::shell::Model) -> Seen {
        Seen {
            pins: model.map_pins(),
            tracks: model.track_input(),
            track_window: model.track_window,
            notice: None,
            loading: None,
            maps: 0,
            autoload: false,
            style: Style::SCREEN,
            scene: model.scene(LayoutClass::Wide),
            now: model.now,
            receipts: model.reading.on,
            photo_size: model.photo_size,
            mailbox: mailbox_lines(model),
            photo: None,
            browse: None,
            copied: None,
            hubs: loquax::ui::settings::hub_rows(model, loquax::ui::settings::Target::Window),
        }
    }

    /// 1371: the desktop's position sheet. Without a fix it offers the
    /// entry (the phone's "turn location on" is no row here); the entry
    /// opens the field, pixel for pixel the golden; Enter hands the typed
    /// position to the model, after which the sheet offers the shares,
    /// the golden too, and "now" sends the report. Once the map was shown
    /// its centre is a row, and a press on it makes that the fix.
    #[test]
    fn the_position_sheet_takes_a_typed_position_and_the_maps_centre() {
        let (w, h) = (900, 560);
        let mut model = chat_model();
        model.bar.abilities = loquax::ui::compose::Abilities::desktop();
        model.changed_outside();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(Frames::new(loquax::picture::Maps::default()), Some(tx));
        app.window = Size::new(w as f32, h as f32);
        let seen = |app: &mut App, model: &mut crate::shell::Model, rx: &mut _| {
            for event in told(rx) {
                update(model, event);
            }
            app.apply(Message::Seen(Box::new(seen_of(model))));
        };
        seen(&mut app, &mut model, &mut rx);
        let _ = app.update(Message::Intent(Intent::Locate { peer: ANNA }));
        seen(&mut app, &mut model, &mut rx);
        let rows = |app: &App| -> Vec<StrId> {
            let sheet = app.scene.sheet.as_ref().expect("the sheet");
            sheet.actions.iter().map(|a| a.label_key).collect()
        };
        assert_eq!(rows(&app), [StrId::EnterPosition], "no map yet, no centre");

        let _ = app.update(Message::Intent(Intent::EnterPosition { text: None }));
        assert_eq!(app.position.as_deref(), Some(""));
        let _ = app.update(Message::Position("52.5163, 13.3777".to_string()));
        map::tests::same_as("gui-position-entry-light.png", &shoot(&app, w, h), w, h);
        let _ = app.update(Message::Intent(Intent::EnterPosition {
            text: app.position.clone(),
        }));
        assert_eq!(app.position, None);
        seen(&mut app, &mut model, &mut rx);
        let fix = model.bar.own_fix.expect("the typed fix");
        assert_eq!(
            (fix.latitude_e6, fix.longitude_e6),
            (52_516_300, 13_377_700)
        );
        assert_eq!(
            rows(&app),
            [
                StrId::ShareNow,
                StrId::ShareFor15,
                StrId::ShareFor1h,
                StrId::ShareFor8h,
                StrId::ShareUntilStopped,
                StrId::EnterPosition
            ]
        );
        map::tests::same_as("gui-position-sheet-light.png", &shoot(&app, w, h), w, h);
        let sent = update(&mut model, UiEvent::ShareFix { peer: ANNA });
        assert!(
            sent.iter()
                .any(|e| matches!(e, loquax::ui::UiEffect::Send { to, .. } if *to == ANNA)),
            "the report goes: {sent:?}"
        );

        app.map_shown = true;
        let _ = app.update(Message::Intent(Intent::Locate { peer: ANNA }));
        seen(&mut app, &mut model, &mut rx);
        assert_eq!(rows(&app).last(), Some(&StrId::MapCentre));
        let _ = app.update(Message::Intent(Intent::FixAtMapCentre));
        seen(&mut app, &mut model, &mut rx);
        let centre = loquax::ui::by_hand::of_world(app.view.centre, 0);
        let fix = model.bar.own_fix.expect("the centre");
        assert_eq!(
            (fix.latitude_e6, fix.longitude_e6),
            (centre.latitude_e6, centre.longitude_e6)
        );
    }

    /// 1371: a bubble's menu forwards it: Forward lists the
    /// conversations, a press on one hands the copy to the model, which
    /// sends it there. Control: the menu without Forward pressed lists
    /// none.
    #[test]
    fn a_bubble_is_forwarded_to_the_conversation_picked() {
        let mut model = chat_model();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(Frames::new(loquax::picture::Maps::default()), Some(tx));
        app.window = Size::new(900.0, 560.0);
        app.apply(Message::Seen(Box::new(seen_of(&model))));
        let key = bubble_key(&app, [2; 32]);
        let bubble = NodeId::Bubble(key);
        let _ = app.update(Message::Intent(Intent::Menu(Some(bubble))));
        assert_eq!(app.forwarding, None, "control: no list yet");
        let _ = app.update(Message::ForwardPick(bubble));
        assert_eq!(app.forwarding, Some(bubble));
        let forward = Intent::Forward {
            from: ANNA,
            key,
            to: BERT,
        };
        let _ = app.update(Message::Intent(forward));
        assert_eq!((app.menu, app.forwarding), (None, None));
        let told = told(&mut rx);
        assert_eq!(
            told,
            [UiEvent::Forward {
                from: ANNA,
                key,
                to: BERT
            }]
        );
        let sent: Vec<_> = told
            .into_iter()
            .flat_map(|event| update(&mut model, event))
            .collect();
        assert!(
            sent.iter().any(|e| matches!(
                e,
                loquax::ui::UiEffect::Send { to, body, .. } if *to == BERT && body == "Good morning!"
            )),
            "{sent:?}"
        );
    }

    /// The profile's facts for the window's and the terminal's goldens
    /// (1443): a client of a mailbox on a server, connected over the
    /// relay, and two devices.
    pub(crate) fn profile_facts() -> loquax::ui::profile::ProfileFacts {
        use loquax::ui::profile::{Device, Mailbox, ProfileFacts, Rung};
        ProfileFacts {
            name: "Mia Sander".to_string(),
            mailbox: Mailbox::Away {
                name: Some("Mia's computer".to_string()),
                rung: Some(Rung::Relayed),
                word: Some(loquax::home::board::Word::Connected),
            },
            devices: vec![
                Device {
                    name: "Laptop".to_string(),
                    this: true,
                },
                Device {
                    name: "Pixel 8".to_string(),
                    this: false,
                },
            ],
            ..Default::default()
        }
    }

    /// 1443: a click on the reader's avatar opens the profile the scene
    /// carries, pixel for pixel the golden, its QR reading back as the
    /// code; "Share my address" hands the code to the clipboard and says
    /// so, and Back closes the profile. Control: before the click no
    /// profile is in the scene.
    #[test]
    fn a_click_on_the_avatar_opens_the_profile() {
        let (w, h) = (900, 560);
        let mut model = chat_model();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(Frames::new(loquax::picture::Maps::default()), Some(tx));
        app.window = Size::new(w as f32, h as f32);
        let private: [u8; 64] = core::array::from_fn(|i| i as u8 + 9);
        let key = leviculum_core::Identity::from_private_key_bytes(&private)
            .expect("a key")
            .public_key_bytes();
        let code = loquax::lxma::encode(&key).expect("a code");
        model.own_code = Some(loquax::ui::OwnCode::of(
            code.clone(),
            loquax::home::qr::verbatim_modules(&code).ok(),
        ));
        model.profile = profile_facts();
        app.apply(Message::Seen(Box::new(seen_of(&model))));
        assert!(app.scene.profile.is_none(), "control: closed");
        let open = app
            .scene
            .action(NodeId::Me)
            .expect("the avatar does something")
            .intent
            .clone();
        assert_eq!(open, Intent::Profile);
        let _ = app.update(Message::Intent(open));
        for event in told(&mut rx) {
            update(&mut model, event);
        }
        app.apply(Message::Seen(Box::new(seen_of(&model))));
        assert!(app.scene.profile.is_some());
        let shot = shoot(&app, w, h);
        map::tests::same_as("gui-profile-light.png", &shot, w, h);
        let luma: Vec<u8> = shot.chunks(3).map(|px| px[1]).collect();
        let read = loquax::scan::decode(w as usize, h as usize, w as usize, &luma);
        assert_eq!(read, [code], "the QR reads as the code");

        let _ = app.update(Message::Intent(Intent::ShareCode));
        assert_eq!(
            app.seen.notice.as_deref(),
            Some(StrId::ProfileCopied.text(app.lang))
        );
        let _ = app.update(Message::Intent(Intent::Back));
        for event in told(&mut rx) {
            update(&mut model, event);
        }
        app.apply(Message::Seen(Box::new(seen_of(&model))));
        assert!(app.scene.profile.is_none());
    }

    /// 1371: a right click on a row opens its menu with the scene's verbs
    /// and the contact's; pin and mute reach the model, a forget asks
    /// once, a rename takes its field. The help opens and Back closes it;
    /// the settings' photo size is told to the model.
    #[test]
    fn a_rows_menu_reaches_the_model_and_the_help_and_photo_size_are_there() {
        let mut model = chat_model();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(Frames::new(loquax::picture::Maps::default()), Some(tx));
        app.window = Size::new(900.0, 560.0);
        app.apply(Message::Seen(Box::new(seen_of(&model))));
        let row = NodeId::Row(ANNA);
        let _ = app.update(Message::Intent(Intent::Menu(Some(row))));
        assert_eq!(app.menu, Some(row));
        let pin = Intent::Pin {
            peer: loquax::ui::intent::Target::Peer(ANNA),
            on: loquax::ui::intent::Switch::On,
        };
        let _ = app.update(Message::Intent(pin));
        assert_eq!(app.menu, None, "a verb closes the menu");
        for event in told(&mut rx) {
            update(&mut model, event);
        }
        assert!(model.peer_state(&ANNA).pinned);
        let mute = Intent::Mute {
            peer: loquax::ui::intent::Target::Peer(ANNA),
            secs: -1,
        };
        let _ = app.update(Message::Intent(mute));
        for event in told(&mut rx) {
            update(&mut model, event);
        }
        assert!(model.muted(&ANNA));

        let _ = app.update(Message::Intent(Intent::Menu(Some(row))));
        let forget = Intent::Forget {
            peer: loquax::ui::intent::Target::Peer(ANNA),
            everywhere: false,
            confirmed: false,
        };
        let _ = app.update(Message::Intent(forget));
        assert_eq!(app.deleting, Some(row), "it asks once");
        assert!(told(&mut rx).is_empty(), "control: nothing forgotten yet");
        let _ = app.update(Message::Intent(Intent::Rename {
            peer: loquax::ui::intent::Target::Peer(ANNA),
            name: None,
        }));
        assert_eq!(
            app.renaming.as_ref().map(|(p, _)| *p),
            Some(ANNA),
            "the field opens"
        );

        let _ = app.update(Message::Intent(Intent::Help));
        assert_eq!(app.help, Some(0));
        let _ = app.update(Message::Intent(Intent::Back));
        assert_eq!(app.help, None);

        let small = loquax::ui::photos::PhotoSize::Small;
        let _ = app.update(Message::Intent(Intent::PhotoSize(small)));
        assert_eq!(told(&mut rx), [UiEvent::PhotoSize(small)]);
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
        leviculum_ui_iced::headless::render(
            app.view(),
            &app.theme(),
            chat::colour(app.tokens().text),
            chat::colour(app.tokens().background),
            w,
            h,
        )
        .expect("the software renderer")
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

    /// 1452: a second Bert writes, so both Berts carry their hash tail and
    /// the sentence beside the name in the list; the open Bert's header
    /// says it after the tail. Anna, whose name nobody shares, is her name
    /// alone.
    #[test]
    fn a_shared_name_is_the_windows_golden() {
        let (w, h) = (900, 560);
        let other_bert = [0xbe; 16];
        let mut model = chat_model();
        model.names.remember(other_bert, "bert");
        update(&mut model, UiEvent::Clock(at(30, 9, 50)));
        received(
            &mut model,
            other_bert,
            5,
            "Which relay, the one by the lake?",
        );
        update(&mut model, UiEvent::Open { peer: BERT });
        let scene = model.scene(LayoutClass::Wide);
        let list = scene.list.as_ref().expect("the list");
        let names: Vec<&str> = list.rows.iter().map(|r| r.row.name.as_str()).collect();
        assert!(names.contains(&"Anna Schulz"), "{names:?}");
        assert!(names.iter().any(|n| n.starts_with("Bert ·")), "{names:?}");
        assert!(names.iter().any(|n| n.starts_with("bert ·")), "{names:?}");
        let said: Vec<String> = list
            .rows
            .iter()
            .filter_map(|r| r.same_name.as_ref().map(|t| t.resolve(Lang::En)))
            .collect();
        assert_eq!(said.len(), 2, "{said:?}");
        let mut app = App::new(Frames::new(loquax::picture::Maps::default()), None);
        app.apply(Message::Seen(Box::new(seen_of(&model))));
        app.window = Size::new(w as f32, h as f32);
        let shot = shoot(&app, w, h);
        map::tests::same_as("gui-same-name.png", &shot, w, h);
    }

    /// A key pressed with Ctrl, or alone (1444).
    fn chord(key: keyboard::Key, ctrl: bool) -> Message {
        Message::Key(keyboard::Event::KeyPressed {
            key: key.clone(),
            modified_key: key,
            physical_key: keyboard::key::Physical::Unidentified(
                keyboard::key::NativeCode::Unidentified,
            ),
            location: keyboard::Location::Standard,
            modifiers: match ctrl {
                true => keyboard::Modifiers::CTRL,
                false => keyboard::Modifiers::empty(),
            },
            text: None,
            repeat: false,
        })
    }

    /// The presses `n` clicks at `at` on the window's view made, as the
    /// bubble's key and the press.
    fn presses(app: &App, at: Point, n: usize, w: u32, h: u32) -> Vec<(RunKey, Pick)> {
        leviculum_ui_iced::headless::clicks(app.view(), at, n, w, h)
            .expect("a renderer")
            .into_iter()
            .filter_map(|m| match m {
                Message::PickText(key, pick) => Some((key, pick)),
                _ => None,
            })
            .collect()
    }

    /// Where on the 900 x 560 window a single click lands in Anna's
    /// "Good morning!" on the word `word`: her bubbles are at the chat's
    /// left, so a column just inside them is walked down to the bubble,
    /// then its line to the right to the word.
    fn spot_of(app: &App, word: &str) -> Point {
        let (w, h) = (900, 560);
        let runs = app.text_runs();
        let text_of = |key: RunKey| {
            runs.iter()
                .find(|r| r.key == key)
                .map(|r| r.text.clone())
                .unwrap_or_default()
        };
        let x0 = chat::RAIL + chat::LIST_WIDTH + 30.0;
        for y in (60..h - 60).step_by(3) {
            let y = y as f32;
            let Some((key, Pick::Press { .. })) =
                presses(app, Point::new(x0, y), 1, w, h).first().copied()
            else {
                continue;
            };
            if text_of(key) != "Good morning!" {
                continue;
            }
            for x in (0..300).step_by(3) {
                let at = Point::new(x0 + x as f32, y);
                if let Some((key, Pick::Press { at: byte, .. })) =
                    presses(app, at, 1, w, h).first().copied()
                {
                    let text = text_of(key);
                    if &text[loquax::ui::selection::word_at(&text, byte)] == word {
                        return at;
                    }
                }
            }
        }
        panic!("no \"{word}\" in Anna's bubble");
    }

    /// A double click on a word of a bubble selects the word (1444): the
    /// word goes onto the PRIMARY selection at once, Ctrl+C puts it on the
    /// clipboard, both read from the test clipboard. A triple click is the
    /// bubble's line; a plain click clears, and Ctrl+C then copies
    /// nothing; Escape clears too.
    #[test]
    fn a_double_click_on_a_word_selects_it_and_ctrl_c_copies_it() {
        let (w, h) = (900, 560);
        let (mut app, _rx) = chat_window();
        app.window = Size::new(w as f32, h as f32);
        app.rescene();
        let at = spot_of(&app, "morning");
        for (key, pick) in presses(&app, at, 2, w, h) {
            let _ = app.update(Message::PickText(key, pick));
        }
        assert_eq!(
            app.clips,
            vec![(select::Buffer::Primary, "morning".to_string())]
        );
        assert!(!app.picking, "the release ended the press");
        let _ = app.update(chord(keyboard::Key::Character("c".into()), true));
        assert_eq!(
            app.clips.last(),
            Some(&(select::Buffer::Clipboard, "morning".to_string()))
        );
        // A triple click: the whole line.
        app.clips.clear();
        for (key, pick) in presses(&app, at, 3, w, h) {
            let _ = app.update(Message::PickText(key, pick));
        }
        assert_eq!(
            app.clips.last(),
            Some(&(select::Buffer::Primary, "Good morning!".to_string()))
        );
        // A plain click clears: Ctrl+C has nothing to copy, and the key
        // goes on to the keymap.
        for (key, pick) in presses(&app, at, 1, w, h) {
            let _ = app.update(Message::PickText(key, pick));
        }
        app.clips.clear();
        let _ = app.update(chord(keyboard::Key::Character("c".into()), true));
        assert!(app.clips.is_empty(), "{:?}", app.clips);
        // Escape clears a selection and is spent on it; without one it
        // goes on.
        let _ = app.update(chord(keyboard::Key::Character("a".into()), true));
        assert!(app.picked.is_some());
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
        assert!(app.selection_key(&escape).is_some());
        assert_eq!(app.picked, None);
        assert!(app.selection_key(&escape).is_none());
    }

    /// A press in one bubble and a drag into a later one selects across
    /// them in reading order (1444); the PRIMARY selection follows each
    /// change, and the copy names the writer and the time on each line.
    /// Shift+click moves the end; Ctrl+A is the whole conversation.
    #[test]
    fn a_drag_across_bubbles_copies_one_line_per_bubble() {
        let (mut app, _rx) = chat_window();
        let runs: Vec<_> = (app.text_runs().into_iter())
            .filter(|r| r.key.part == Part::Body)
            .collect();
        let texts: Vec<&str> = runs.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                "Are you coming to the market tomorrow?",
                "Good morning!",
                "We meet at the fountain at ten, the stall with the apples.",
                "Yes, see you there",
            ]
        );
        let (first, second, third) = (runs[0].key, runs[1].key, runs[2].key);
        let _ = app.update(Message::PickText(
            first,
            Pick::Press {
                at: 22,
                clicks: 1,
                shift: false,
            },
        ));
        assert!(app.clips.is_empty(), "a caret selects nothing");
        let _ = app.update(Message::PickText(second, Pick::Drag { at: 4 }));
        let _ = app.update(Message::PickText(third, Pick::Drag { at: 7 }));
        let _ = app.update(Message::PickText(first, Pick::Release));
        let (_, last) = app.clips.last().cloned().expect("a primary write");
        let anna = &runs[0].who;
        let line = |n: usize| format!("{anna}, {}", runs[n].at.clone().unwrap_or_default());
        assert_eq!(
            last,
            format!(
                "{}: market tomorrow?\nToday\n{}: Good morning!\n{}: We meet",
                line(0),
                line(1),
                line(2)
            ),
            "the day line between selects too (1475), alone on its line"
        );
        assert_eq!(app.clips.len(), 2, "one write per change: {:?}", app.clips);
        // After the release a drag no longer moves it.
        let _ = app.update(Message::PickText(third, Pick::Drag { at: 20 }));
        assert_eq!(app.clips.len(), 2);
        // Shift+click moves the end back into the first bubble.
        let _ = app.update(Message::PickText(
            first,
            Pick::Press {
                at: 28,
                clicks: 1,
                shift: true,
            },
        ));
        assert_eq!(app.clips.last().map(|c| c.1.as_str()), Some("market"));
        // Ctrl+A: every bubble.
        let _ = app.update(chord(keyboard::Key::Character("a".into()), true));
        let (_, all) = app.clips.last().cloned().expect("a write");
        let days = (app.text_runs().iter())
            .filter(|r| r.key.part == Part::Day)
            .count();
        assert_eq!(all.lines().count(), 4 + days, "{all}");
        assert!(all.ends_with("Yes, see you there"), "{all}");
    }

    /// The chat with a selection drawn (1444), pixel for pixel the golden,
    /// light: "morning" picked in Anna's bubble, the drag from "market"
    /// into the next. Without a selection it is the chat's own golden.
    #[test]
    fn a_selection_is_drawn_under_the_words_as_the_golden() {
        let (w, h) = (900, 560);
        let (mut app, _rx) = chat_window();
        app.window = Size::new(w as f32, h as f32);
        app.rescene();
        let plain = shoot(&app, w, h);
        map::tests::same_as("gui-chat-light.png", &plain, w, h);
        let runs: Vec<_> = (app.text_runs().into_iter())
            .filter(|r| r.key.part == Part::Body)
            .collect();
        let _ = app.update(Message::PickText(
            runs[0].key,
            Pick::Press {
                at: 22,
                clicks: 1,
                shift: false,
            },
        ));
        let _ = app.update(Message::PickText(runs[1].key, Pick::Drag { at: 12 }));
        let _ = app.update(Message::PickText(runs[0].key, Pick::Release));
        let picked = shoot(&app, w, h);
        assert_ne!(picked, plain);
        map::tests::same_as("gui-chat-selected-light.png", &picked, w, h);
    }

    /// The same selection in the dark theme (1475): the highlight is the
    /// bubble's ink, faint, and shows on the dark grounds too.
    #[test]
    fn a_selection_is_drawn_in_the_dark_theme_as_the_golden() {
        let (w, h) = (900, 560);
        let (mut app, _rx) = chat_window();
        app.window = Size::new(w as f32, h as f32);
        let _ = app.update(Message::Intent(Intent::Theme));
        assert!(app.dark());
        app.rescene();
        let plain = shoot(&app, w, h);
        let runs: Vec<_> = (app.text_runs().into_iter())
            .filter(|r| r.key.part == Part::Body)
            .collect();
        let _ = app.update(Message::PickText(
            runs[0].key,
            Pick::Press {
                at: 22,
                clicks: 1,
                shift: false,
            },
        ));
        let _ = app.update(Message::PickText(runs[1].key, Pick::Drag { at: 12 }));
        let _ = app.update(Message::PickText(runs[0].key, Pick::Release));
        let picked = shoot(&app, w, h);
        assert_ne!(picked, plain, "the highlight shows on the dark grounds");
        map::tests::same_as("gui-chat-selected-dark.png", &picked, w, h);
    }

    /// `chat_window` with more kinds of text (1475): Anna's last bubble
    /// quotes her first, "Good morning!" carries a position, and a
    /// group's own line stands after it. The scene is the model's,
    /// changed where the model has no quick way to say it.
    fn rich_window() -> (App, tokio::sync::mpsc::UnboundedReceiver<Want>) {
        let (mut app, rx) = chat_window();
        let chat = app.seen.scene.chat.as_mut().expect("Anna's chat");
        let mut note = None;
        for group in &mut chat.groups {
            for node in &mut group.bubbles {
                match node.text.as_deref() {
                    Some("We meet at the fountain at ten, the stall with the apples.") => {
                        node.bubble.quote = Some(loquax::ui::replies::Quoted {
                            text: "Are you coming to the market tomorrow?".to_string(),
                            ours: Some(false),
                            to: [1; 32],
                        });
                    }
                    Some("Good morning!") => {
                        let location = leviculum_lxmf::telemetry::Location {
                            latitude_e6: 52_520_000,
                            longitude_e6: 13_405_000,
                            altitude_e2: 0,
                            speed_e2: 0,
                            bearing_e2: 0,
                            accuracy_e2: 500,
                            last_update: 0,
                        };
                        let fix = loquax::telemetry::Fix {
                            peer: ANNA,
                            via: ANNA,
                            message: None,
                            received_at: None,
                            location,
                            telemetry: Vec::new(),
                            source: loquax::telemetry::Source::Telemetry,
                        };
                        // The scene's sentence with it (1449 item 5).
                        node.position = Some(loquax::ui::scene::position_words(&fix, false, None));
                        node.bubble.fix = Some(fix);
                        let mut line = node.clone();
                        line.bubble.note = true;
                        line.bubble.key = 990;
                        line.bubble.body = "Bert joined.".to_string();
                        note = Some(line);
                    }
                    _ => {}
                }
            }
        }
        let note = note.expect("Good morning");
        let group = (chat.groups.iter_mut())
            .find(|g| {
                g.bubbles
                    .iter()
                    .any(|n| n.text.as_deref() == Some("Good morning!"))
            })
            .expect("its group");
        let at = (group.bubbles.iter())
            .position(|n| n.text.as_deref() == Some("Good morning!"))
            .expect("it");
        group.bubbles.insert(at + 1, note);
        app.window = Size::new(900.0, 560.0);
        app.rescene();
        (app, rx)
    }

    /// Where on the window a double click lands on a text of `part`, and
    /// the run and the byte it pressed: a few columns walked down, the
    /// list's and the chat's left, middle and right (1475).
    fn find_part(app: &App, part: Part, w: u32, h: u32) -> (Point, RunKey, usize) {
        let list = chat::RAIL + 90.0;
        let chat = chat::RAIL + chat::LIST_WIDTH;
        let width = w as f32 - chat;
        let columns = [
            list,
            chat + 30.0,
            chat + 60.0,
            chat + width / 2.0 - 10.0,
            chat + width / 2.0,
            chat + width - 120.0,
            chat + width - 60.0,
        ];
        for x in columns {
            for y in (40..h - 40).step_by(3) {
                let at = Point::new(x, y as f32);
                let hit =
                    presses(app, at, 2, w, h)
                        .into_iter()
                        .find_map(|(key, pick)| match pick {
                            Pick::Press {
                                at: byte,
                                clicks: 2,
                                ..
                            } if key.part == part => Some((key, byte)),
                            _ => None,
                        });
                // On a word, not on the blank between two.
                if let Some((key, byte)) = hit.filter(|(key, byte)| on_a_word(app, *key, *byte)) {
                    return (at, key, byte);
                }
            }
        }
        panic!("no {part:?} on the window");
    }

    /// Whether byte `byte` of the run `key` is in a word.
    fn on_a_word(app: &App, key: RunKey, byte: usize) -> bool {
        (app.text_runs().iter().find(|r| r.key == key)).is_some_and(|run| {
            !run.text[loquax::ui::selection::word_at(&run.text, byte)]
                .trim()
                .is_empty()
        })
    }

    /// Every kind of text in the window selects on a double click and
    /// copies (1475): the list's last line, the day line, the quote, the
    /// words, the position line and a group's own line. The word goes onto
    /// PRIMARY, Ctrl+C onto the clipboard, both read from the test
    /// clipboard; what is copied is the run's word, as the core says.
    #[test]
    fn every_kind_of_window_text_selects_on_a_double_click_and_copies() {
        let (w, h) = (900, 560);
        let (mut app, _rx) = rich_window();
        let parts = [
            Part::Preview,
            Part::Day,
            Part::Quote,
            Part::Body,
            Part::Position,
            Part::Note,
        ];
        for part in parts {
            app.picked = None;
            app.clips.clear();
            let (at, key, byte) = find_part(&app, part, w, h);
            let runs = app.text_runs();
            let run = runs.iter().find(|r| r.key == key).expect("its run");
            let word = run.text[loquax::ui::selection::word_at(&run.text, byte)].to_string();
            assert!(
                !word.trim().is_empty(),
                "{part:?}: {word:?} in {:?}",
                run.text
            );
            for (key, pick) in presses(&app, at, 2, w, h) {
                let _ = app.update(Message::PickText(key, pick));
            }
            assert_eq!(
                app.clips.last(),
                Some(&(select::Buffer::Primary, word.clone())),
                "{part:?} at {at:?}"
            );
            let _ = app.update(chord(keyboard::Key::Character("c".into()), true));
            assert_eq!(
                app.clips.last(),
                Some(&(select::Buffer::Clipboard, word.clone())),
                "{part:?}"
            );
        }
        // The texts are the scene's: the day, the quote, the line.
        let runs = app.text_runs();
        let of = |part: Part| -> Vec<String> {
            (runs.iter())
                .filter(|r| r.key.part == part)
                .map(|r| r.text.clone())
                .collect()
        };
        assert_eq!(
            of(Part::Quote),
            vec!["Are you coming to the market tomorrow?"]
        );
        assert_eq!(of(Part::Note), vec!["Bert joined."]);
        // One sentence (1449 item 5); the coordinates are the Info's.
        assert!(
            of(Part::Position)[0].contains("osition") && !of(Part::Position)[0].contains("52.52"),
            "{:?}",
            of(Part::Position)
        );
        assert_eq!(
            of(Part::Preview).len(),
            2,
            "both rows: {:?}",
            of(Part::Preview)
        );
    }

    /// A list row and a position line are buttons (1475): a plain click
    /// opens as before and selects nothing; a drag along the line selects
    /// and its release opens nothing, the button's open the release sent
    /// dropped; and the drop does not outlive that release.
    #[test]
    fn a_click_on_a_row_opens_and_a_drag_on_its_line_selects() {
        let (w, h) = (900, 560);
        let (mut app, mut rx) = rich_window();
        for part in [Part::Preview, Part::Position] {
            let (at, _, _) = find_part(&app, part, w, h);
            let _ = told(&mut rx);
            // A click: the button's open, no selection.
            let click =
                leviculum_ui_iced::headless::clicks(app.view(), at, 1, w, h).expect("a renderer");
            assert!(
                !click.iter().any(|m| matches!(m, Message::PickText(..))),
                "{part:?}"
            );
            let opens = click
                .iter()
                .filter(|m| matches!(m, Message::Intent(_)))
                .count();
            assert_eq!(opens, 1, "{part:?}: the click opens");
            // A drag of 40 pixels along it.
            let to = Point::new(at.x + 40.0, at.y);
            let drag =
                leviculum_ui_iced::headless::drag(app.view(), at, to, 8, w, h).expect("a renderer");
            let picks = drag
                .iter()
                .filter(|m| matches!(m, Message::PickText(..)))
                .count();
            assert!(picks >= 3, "{part:?}: a press, a drag, the release");
            app.clips.clear();
            let mut opened = false;
            for message in drag {
                let intent = matches!(message, Message::Intent(_));
                let before = app.screen;
                let _ = app.update(message);
                opened |= intent && app.screen != before;
            }
            assert!(!opened, "{part:?}: the drag's release opened");
            let (others, _) = told_apart(&mut rx);
            assert!(others.is_empty(), "{part:?}: {others:?}");
            let (buffer, words) = app.clips.last().cloned().expect("a selection");
            assert_eq!(buffer, select::Buffer::Primary);
            assert!(!words.is_empty(), "{part:?}");
            // The drop is gone with the view after it: a click opens.
            let _ = app.view();
            for message in click {
                let _ = app.update(message);
            }
            let (others, _) = told_apart(&mut rx);
            let mapped = app.screen == Screen::Map;
            assert!(
                !others.is_empty() || mapped,
                "{part:?}: the click opened nothing"
            );
            // Back to the chat for the next part.
            app.screen = Screen::Chats;
            app.picked = None;
        }
    }

    /// The address and the code in the profile select (1475): a double
    /// click on the address takes a four, Ctrl+A takes both, and the
    /// address is copied without the blanks between its fours.
    #[test]
    fn the_profiles_address_and_code_select_and_copy() {
        let (w, h) = (900, 560);
        let mut model = chat_model();
        let mut app = App::new(Frames::new(loquax::picture::Maps::default()), None);
        app.window = Size::new(w as f32, h as f32);
        let private: [u8; 64] = core::array::from_fn(|i| i as u8 + 9);
        let key = leviculum_core::Identity::from_private_key_bytes(&private)
            .expect("a key")
            .public_key_bytes();
        let code = loquax::lxma::encode(&key).expect("a code");
        model.own_code = Some(loquax::ui::OwnCode::of(
            code.clone(),
            loquax::home::qr::verbatim_modules(&code).ok(),
        ));
        model.profile = profile_facts();
        update(&mut model, UiEvent::Profile { open: true });
        app.apply(Message::Seen(Box::new(seen_of(&model))));
        let profile = app.scene.profile.clone().expect("the profile");
        let runs = app.text_runs();
        assert_eq!(
            runs.iter().map(|r| r.key.part).collect::<Vec<_>>(),
            vec![Part::Address, Part::Code],
            "the profile's texts alone, nothing under it"
        );
        for part in [Part::Address, Part::Code] {
            let (at, key, byte) = find_profile_part(&app, part, w, h);
            let run = runs.iter().find(|r| r.key == key).expect("its run");
            let word = run.text[loquax::ui::selection::word_at(&run.text, byte)].to_string();
            app.clips.clear();
            for (key, pick) in presses(&app, at, 2, w, h) {
                let _ = app.update(Message::PickText(key, pick));
            }
            assert_eq!(
                app.clips.last(),
                Some(&(select::Buffer::Primary, word.clone())),
                "{part:?}"
            );
        }
        let _ = app.update(chord(keyboard::Key::Character("a".into()), true));
        let _ = app.update(chord(keyboard::Key::Character("c".into()), true));
        assert_eq!(
            app.clips.last(),
            Some(&(
                select::Buffer::Clipboard,
                format!("{}\n{code}", profile.fours.replace(' ', ""))
            ))
        );
    }

    /// [`find_part`] over the profile in the window's middle.
    fn find_profile_part(app: &App, part: Part, w: u32, h: u32) -> (Point, RunKey, usize) {
        for x in (100..w - 100).step_by(20) {
            for y in (20..h - 20).step_by(3) {
                let at = Point::new(x as f32, y as f32);
                let hit =
                    presses(app, at, 2, w, h)
                        .into_iter()
                        .find_map(|(key, pick)| match pick {
                            Pick::Press {
                                at: byte,
                                clicks: 2,
                                ..
                            } if key.part == part => Some((key, byte)),
                            _ => None,
                        });
                if let Some((key, byte)) = hit {
                    return (at, key, byte);
                }
            }
        }
        panic!("no {part:?} in the profile");
    }

    /// A middle click on the compose field pastes the PRIMARY selection
    /// where it clicked (1475): at the field's start the words go before
    /// what was typed. The control: with no pointer known the paste goes
    /// where the cursor was, after it.
    #[test]
    fn a_middle_click_pastes_where_it_clicked() {
        let (mut app, _rx) = chat_window();
        type_into(&mut app, "hello world");
        let _ = app.update(Message::PrimaryText(Some("X".to_string())));
        assert_eq!(app.compose.text().trim_end(), "hello worldX", "the control");
        let (mut app, _rx) = chat_window();
        type_into(&mut app, "hello world");
        // The field laid out, as the window has it before any click.
        let _ = shoot(&app, 900, 560);
        let _ = app.update(Message::ComposePointer(Point::new(
            chat::COMPOSE_PAD_X,
            chat::COMPOSE_PAD_Y + 2.0,
        )));
        let _ = app.update(Message::PastePrimary);
        let _ = app.update(Message::PrimaryText(Some("X".to_string())));
        assert_eq!(app.compose.text().trim_end(), "Xhello world");
    }

    /// The window over Anna's chat with `chat_model`'s messages and 3000
    /// more, open, `w` by `h` (1431, the window bench of 1441).
    pub(super) fn long_chat(w: u32, h: u32) -> App {
        let model = long_chat_model();
        // The channel's receiver dropped: what the window tells the model
        // goes nowhere.
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(Frames::new(loquax::picture::Maps::default()), Some(tx));
        app.window = Size::new(w as f32, h as f32);
        app.apply(Message::Seen(Box::new(seen_of(&model))));
        app
    }

    /// [`long_chat`]'s model: Anna's chat with 3000 more, open.
    pub(super) fn long_chat_model() -> crate::shell::Model {
        let mut model = chat_model();
        for i in 0..3000u32 {
            let mut id = [7u8; 32];
            id[..4].copy_from_slice(&i.to_be_bytes());
            update(
                &mut model,
                UiEvent::Engine(OutboxEvent::Received {
                    sent_at: 0,
                    message_id: id,
                    source: ANNA,
                    title: Vec::new(),
                    body: format!("Message {i}, a line about the market and the apples.")
                        .into_bytes(),
                    fields: Vec::new(),
                    verified: true,
                }),
            );
        }
        update(&mut model, UiEvent::Open { peer: ANNA });
        model
    }

    /// The window over Anna's chat shaped like Lew's real ones (1456): not
    /// thousands of one-line bubbles but a few hundred messages, most of
    /// them long and wrapping over several lines, runs of hers and of
    /// ours, every fourth of ours a reply quoting her last, emoji here and
    /// there, the days going by; open, `w` by `h`. Deterministic.
    pub(super) fn real_chat(w: u32, h: u32) -> App {
        let mut model = real_chat_grown(|_| {});
        update(&mut model, UiEvent::Open { peer: ANNA });
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(Frames::new(loquax::picture::Maps::default()), Some(tx));
        app.window = Size::new(w as f32, h as f32);
        app.apply(Message::Seen(Box::new(seen_of(&model))));
        app
    }

    /// The model of [`real_chat`], `each` handed it after every message,
    /// as a first pull brings them one after the other (1464).
    pub(super) fn real_chat_grown(
        mut each: impl FnMut(&mut crate::shell::Model),
    ) -> crate::shell::Model {
        const WORDS: [&str; 24] = [
            "the relay",
            "on the hill",
            "came back",
            "after the storm",
            "and the battery",
            "held the night",
            "through",
            "I moved",
            "the antenna",
            "two metres higher",
            "so the link",
            "to the valley",
            "reaches the farm now",
            "but the solar panel",
            "needs cleaning",
            "before winter",
            "tomorrow",
            "maybe",
            "with Bert",
            "if the road is open",
            "🙂",
            "👍",
            "🍎",
            "📡",
        ];
        let mut model = chat_model();
        let mut seed = 0x2545_f491_u32;
        let mut next = |n: u32| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed % n
        };
        let mut sentence = |words: u32| {
            let mut out = String::new();
            for i in 0..words {
                if i > 0 {
                    out.push(' ');
                }
                out.push_str(WORDS[next(WORDS.len() as u32) as usize]);
            }
            out.push('.');
            out
        };
        let mut hers = [0u8; 32];
        let mut ours = 0u32;
        // From ten on the morning of chat_model's last message, a day
        // and a half of talk.
        let mut minute = 10 * 60u32;
        let mut messages = 0u32;
        while messages < 300 {
            // A run of hers, one to three messages, then one of ours, one
            // or two.
            for _ in 0..1 + messages % 3 {
                minute += 7;
                update(
                    &mut model,
                    UiEvent::Clock(at(30 + minute / 1440, minute / 60 % 24, minute % 60)),
                );
                let length = 2 + messages % 5;
                let body = (0..length)
                    .map(|_| sentence(6 + messages % 9))
                    .collect::<Vec<_>>()
                    .join(" ");
                hers = [0u8; 32];
                hers[..4].copy_from_slice(&(10_000 + messages).to_be_bytes());
                update(
                    &mut model,
                    UiEvent::Engine(OutboxEvent::Received {
                        sent_at: 0,
                        message_id: hers,
                        source: ANNA,
                        title: Vec::new(),
                        body: body.into_bytes(),
                        fields: Vec::new(),
                        verified: true,
                    }),
                );
                messages += 1;
                each(&mut model);
            }
            for _ in 0..1 + messages % 2 {
                minute += 3;
                update(
                    &mut model,
                    UiEvent::Clock(at(30 + minute / 1440, minute / 60 % 24, minute % 60)),
                );
                let body = (0..3 + ours % 4)
                    .map(|_| sentence(8 + ours % 7))
                    .collect::<Vec<_>>()
                    .join(" ");
                ours += 1;
                let event = match ours % 4 {
                    0 => UiEvent::Reply {
                        peer: ANNA,
                        target: hers,
                        body,
                    },
                    _ => UiEvent::Write { peer: ANNA, body },
                };
                update(&mut model, event);
                messages += 1;
                each(&mut model);
            }
        }
        model
    }

    /// The bench's real-shaped chat is what its doc says (1456): a few
    /// hundred bubbles, more than half of them over 200 characters, a
    /// quarter of them ours, replies quoting hers, emoji among the words.
    #[test]
    fn the_real_chat_is_shaped_like_lews() {
        let app = real_chat(1276, 2099);
        let chat = app.scene.chat.as_ref().expect("the chat open");
        let bubbles: Vec<_> = chat
            .groups
            .iter()
            .flat_map(|g| g.bubbles.iter().map(|b| &b.bubble))
            .collect();
        let n = bubbles.len();
        assert!((300..400).contains(&n), "{n} bubbles");
        let ours = bubbles.iter().filter(|b| b.ours).count();
        assert!(ours * 4 > n, "{ours} of {n} ours");
        let quoting = bubbles
            .iter()
            .filter(|b| b.quote.as_ref().is_some_and(|q| !q.text.is_empty()))
            .count();
        assert!(quoting > 20, "{quoting} quoting");
        let long = bubbles
            .iter()
            .filter(|b| b.body.chars().count() > 200)
            .count();
        assert!(long * 2 > n, "{long} of {n} long");
        assert!(bubbles.iter().any(|b| b.body.contains('\u{1f642}')));
    }

    /// A long conversation scrolls softly without building the view
    /// (1431): Anna's chat with 3000 more messages, a wheel's notch over
    /// the messages and the screen's frames after it scroll the list on
    /// every frame, eased, and publish no message, so iced redraws the
    /// screen it has and builds no view per frame: the frame costs the
    /// rows in sight. The numbers it prints are the report's: the view
    /// built and laid out once (what a message arriving costs), and a
    /// frame of the scroll.
    #[test]
    fn a_long_chat_scrolls_without_building_the_view() {
        let (w, h) = (900u32, 560u32);
        let app = long_chat(w, h);
        // The chat holds more than 3000 messages: the model's count, not
        // the scene's (1489, review finding 14). A scene that builds only
        // the bubbles on screen is the goal, not a break of this test.
        let held = long_chat_model()
            .conversations
            .iter()
            .find(|c| c.peer == ANNA)
            .map_or(0, |c| c.messages.len());
        assert!(held > 3000, "{held} messages");
        assert!(app.scene.chat.is_some(), "the chat open");
        let (text, ground) = (
            chat::colour(app.tokens().text),
            chat::colour(app.tokens().background),
        );
        let mut screen =
            leviculum_ui_iced::headless::Offscreen::new(w, h).expect("the software renderer");
        let mut built = Vec::new();
        for _ in 0..3 {
            let viewed = Instant::now();
            let view = app.view();
            let viewed = viewed.elapsed().as_secs_f64() * 1e3;
            let (laid, raster) = screen.frame(view, &app.theme(), text, ground);
            built.push((viewed, laid.as_secs_f64() * 1e3, raster.as_secs_f64() * 1e3));
        }
        let start = Instant::now();
        let over = Point::new(w as f32 - 200.0, h as f32 / 2.0);
        let mut events = vec![iced::Event::Mouse(mouse::Event::WheelScrolled {
            delta: mouse::ScrollDelta::Lines { x: 0.0, y: 1.0 },
        })];
        for i in 1..=10u64 {
            events.push(iced::Event::Window(window::Event::RedrawRequested(
                start + Duration::from_millis(16 * i),
            )));
        }
        let (_, before, _) =
            screen.redraws(app.view(), &events[..1], over, &app.theme(), text, ground);
        let (times, after, published) =
            screen.redraws(app.view(), &events, over, &app.theme(), text, ground);
        assert!(published.is_empty(), "a scroll frame publishes nothing");
        assert_ne!(before, after, "the notch scrolled the messages");
        let ms = |d: Duration| d.as_secs_f64() * 1e3;
        for (viewed, laid, raster) in &built {
            println!("LONG_CHAT {held} messages: view built {viewed:.2} ms, laid out {laid:.2} ms, rasterised {raster:.2} ms");
        }
        let frames: Vec<String> = times[1..]
            .iter()
            .map(|(d, r)| format!("{:.2}+{:.2}", ms(*d), ms(*r)))
            .collect();
        println!(
            "LONG_CHAT scroll frames (update and draw + rasterise, ms): {}",
            frames.join(" ")
        );
    }

    /// The window with the desktop's chooser (1341): the round button is
    /// the plus while the field is empty, the plus opens the attachment
    /// sheet over the bar with Gallery and File, pixel for pixel the
    /// golden; a row closes the sheet for the chooser, whose path goes to
    /// the shell as the attachment, a cancel to nothing and a missing
    /// portal to the notice; Esc closes an open sheet.
    #[test]
    fn the_plus_opens_the_desktop_choosers_sheet() {
        use loquax::ui::compose::Round;
        let (w, h) = (900, 560);
        let mut model = chat_model();
        model.bar.abilities.picker = true;
        model.changed_outside();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(Frames::new(loquax::picture::Maps::default()), Some(tx));
        app.window = Size::new(w as f32, h as f32);
        app.apply(Message::Seen(Box::new(seen_of(&model))));
        let compose = &app.scene.chat.as_ref().expect("a chat").compose;
        assert_eq!(compose.round, Round::Plus);
        let peer = ANNA;
        let plus = compose
            .buttons
            .iter()
            .find(|b| b.button == loquax::ui::compose::Button::Plus)
            .map(|b| b.action.intent.clone())
            .expect("the plus");
        let _ = app.update(Message::Intent(plus));
        let opened = told(&mut rx);
        assert_eq!(opened, [UiEvent::OpenSheet(Sheet::Attach { peer })]);
        for event in opened {
            update(&mut model, event);
        }
        app.apply(Message::Seen(Box::new(seen_of(&model))));
        let sheet = app.scene.sheet.clone().expect("the sheet");
        let rows: Vec<_> = sheet.actions.iter().map(|a| a.label_key).collect();
        assert_eq!(rows, [StrId::AttachPhoto, StrId::AttachFile]);
        map::tests::same_as("gui-attach-sheet-light.png", &shoot(&app, w, h), w, h);

        let _ = app.update(Message::Intent(sheet.actions[0].intent.clone()));
        assert_eq!(told(&mut rx), [UiEvent::CloseSheet]);
        let path = std::path::PathBuf::from("/home/mia/Pictures/hill.jpg");
        let _ = app.update(Message::Picked {
            peer,
            photo: true,
            path: Ok(Some(path.clone())),
        });
        match rx.try_recv() {
            Ok(Want::Attach {
                peer: to,
                path: picked,
                photo: true,
            }) => assert_eq!((to, picked), (peer, path)),
            other => panic!("not the attachment: {other:?}"),
        }
        let _ = app.update(Message::Picked {
            peer,
            photo: false,
            path: Ok(None),
        });
        assert!(rx.try_recv().is_err(), "a cancel sends nothing");
        let _ = app.update(Message::Picked {
            peer,
            photo: false,
            path: Err("no portal".to_string()),
        });
        assert_eq!(app.seen.notice.as_deref(), Some("no portal"));

        let _ = app.intent(Intent::Dismiss);
        assert_eq!(told(&mut rx), [UiEvent::CloseSheet]);
    }

    /// The new conversation's picture of a code (1341): the chooser's
    /// picture decoded by the phone's scanner opens the chat of the
    /// address in it; a picture without a code says so.
    #[test]
    fn a_picture_of_a_contact_code_opens_its_chat() {
        let dir = tempfile::tempdir().expect("dir");
        let private: [u8; 64] = core::array::from_fn(|i| i as u8 + 9);
        let key = leviculum_core::Identity::from_private_key_bytes(&private)
            .expect("a key")
            .public_key_bytes();
        let code = loquax::lxma::encode(&key).expect("a code");
        let peer = loquax::home::lxmf_address(&key).expect("an address");
        let (size, modules) = loquax::home::qr::verbatim_modules(&code).expect("modules");
        let (size, scale) = (size as usize, 4);
        let side = ((size + 8) * scale) as u32;
        let mut rgb = Vec::new();
        for y in 0..side as usize {
            for x in 0..side as usize {
                let (mx, my) = (x / scale, y / scale);
                let dark = (4..size + 4).contains(&mx)
                    && (4..size + 4).contains(&my)
                    && modules[(my - 4) * size + mx - 4];
                rgb.extend_from_slice(&[if dark { 10 } else { 240 }; 3]);
            }
        }
        let picture = dir.path().join("code.png");
        let mut png = Vec::new();
        let mut e = png::Encoder::new(&mut png, side, side);
        e.set_color(png::ColorType::Rgb);
        e.set_depth(png::BitDepth::Eight);
        e.write_header()
            .expect("header")
            .write_image_data(&rgb)
            .expect("data");
        std::fs::write(&picture, &png).expect("write");

        let (mut app, mut rx) = chat_window();
        let _ = app.intent(Intent::NewConversation);
        let _ = app.update(Message::CodePicked(Ok(Some(picture))));
        assert_eq!(told(&mut rx), [UiEvent::Open { peer }]);
        assert_eq!(app.new_address, None);

        let blank = dir.path().join("blank.png");
        let mut png = Vec::new();
        let mut e = png::Encoder::new(&mut png, 8, 8);
        e.set_color(png::ColorType::Rgb);
        e.set_depth(png::BitDepth::Eight);
        e.write_header()
            .expect("header")
            .write_image_data(&[200; 8 * 8 * 3])
            .expect("data");
        std::fs::write(&blank, &png).expect("write");
        let _ = app.update(Message::CodePicked(Ok(Some(blank))));
        assert!(app
            .seen
            .notice
            .as_deref()
            .is_some_and(|n| n == "There is no code in this picture."));
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
                    sent_at: 0,
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

    /// 1307 pass two: the four group screens in the window, pixel for
    /// pixel the goldens, light: the group's chat with the writers' names
    /// above their bubbles and the member count as the avatar, Anna's chat
    /// holding the invitation's card with Join, the member sheet, and "New
    /// group" at its name step with the trust sentence. Each press on a
    /// node is the scene's verb.
    #[test]
    fn the_group_screens_are_the_goldens() {
        use leviculum_lxmf::lev::{GroupCopy, Invite, Lev, Member, Members};
        use loquax::ui::groups::Verb;
        use loquax::ui::intent::Target;
        const FAMILIE: [u8; 16] = [0x51; 16];
        const VEREIN: [u8; 16] = [0x52; 16];
        let lev_of = |lev: Lev| {
            let mut fields = Vec::new();
            lev.put_into(&mut fields);
            fields
        };
        let arrive = |model: &mut crate::shell::Model, from, id: u8, body: &str, lev: Lev| {
            update(
                model,
                UiEvent::Engine(OutboxEvent::Received {
                    sent_at: 0,
                    message_id: [id; 32],
                    source: from,
                    title: Vec::new(),
                    body: body.as_bytes().to_vec(),
                    fields: lev_of(lev),
                    verified: true,
                }),
            );
        };
        let invite = |group, name: &str| Lev {
            capabilities: Some(3),
            invite: Some(Invite {
                group,
                public_key: [7; 64],
                name: name.into(),
                admin_name: "Anna".into(),
                epoch: 1,
            }),
            ..Lev::default()
        };
        let copy = |writer| Lev {
            capabilities: Some(2),
            group_copy: Some(GroupCopy {
                sender: writer,
                sent_ms: 1,
                epoch: 2,
            }),
            ..Lev::default()
        };
        let mut model = crate::shell::Model::new(&[]);
        model.address = Some(OWN);
        model.names.remember(OWN, "Mia Sander");
        model.names.remember(ANNA, "Anna Schulz");
        model.names.remember(BERT, "Bert");
        update(&mut model, UiEvent::Clock(at(30, 9, 0)));
        received(&mut model, BERT, 9, "The relay on the hill is up again");
        arrive(
            &mut model,
            ANNA,
            1,
            "Anna invited you to Familie: lxma://…",
            invite(FAMILIE, "Familie"),
        );
        update(
            &mut model,
            UiEvent::Group(Verb::Join {
                group: Target::Peer(FAMILIE),
            }),
        );
        update(&mut model, UiEvent::Clock(at(30, 9, 5)));
        arrive(
            &mut model,
            FAMILIE,
            2,
            "",
            Lev {
                capabilities: Some(3),
                members: Some(Members {
                    epoch: 2,
                    members: vec![
                        Member {
                            address: ANNA,
                            name: "Anna".into(),
                            flags: 2 | 4,
                        },
                        Member {
                            address: OWN,
                            name: "Mia".into(),
                            flags: 4,
                        },
                        Member {
                            address: BERT,
                            name: "Bert".into(),
                            flags: 0,
                        },
                    ],
                    ended: false,
                }),
                ..Lev::default()
            },
        );
        update(&mut model, UiEvent::Clock(at(30, 9, 10)));
        arrive(
            &mut model,
            FAMILIE,
            3,
            "Anna Schulz: Bread is here",
            copy(ANNA),
        );
        update(&mut model, UiEvent::Clock(at(30, 9, 11)));
        arrive(&mut model, FAMILIE, 4, "Bert: Thanks!", copy(BERT));
        update(&mut model, UiEvent::Clock(at(30, 9, 12)));
        update(
            &mut model,
            UiEvent::Write {
                peer: FAMILIE,
                body: "On my way".into(),
            },
        );
        update(&mut model, UiEvent::Clock(at(30, 9, 20)));
        arrive(
            &mut model,
            ANNA,
            5,
            "Anna invited you to Verein: lxma://…",
            invite(VEREIN, "Verein"),
        );
        let (w, h) = (900, 560);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(Frames::new(loquax::picture::Maps::default()), Some(tx));
        app.window = Size::new(w as f32, h as f32);
        let mut shot = |model: &mut crate::shell::Model, name: &str| {
            app.apply(Message::Seen(Box::new(seen_of(model))));
            map::tests::same_as(name, &shoot(&app, w, h), w, h);
        };
        update(&mut model, UiEvent::Open { peer: FAMILIE });
        shot(&mut model, "gui-group-chat.png");
        update(&mut model, UiEvent::Open { peer: ANNA });
        shot(&mut model, "gui-group-invitation.png");
        update(&mut model, UiEvent::Open { peer: FAMILIE });
        update(
            &mut model,
            UiEvent::Group(Verb::Members {
                group: Target::Open,
            }),
        );
        shot(&mut model, "gui-group-members.png");
        update(&mut model, UiEvent::Group(Verb::Close));
        update(&mut model, UiEvent::Group(Verb::New));
        update(&mut model, UiEvent::Group(Verb::Pick(BERT)));
        update(&mut model, UiEvent::Group(Verb::Next));
        update(&mut model, UiEvent::Group(Verb::Name("Market".into())));
        shot(&mut model, "gui-group-new.png");
        // A press on the scene's Create asks the model.
        let create = app
            .scene
            .new_group
            .as_ref()
            .map(|n| n.create.clone())
            .expect("Create");
        assert!(create.enabled);
        assert_eq!(create.intent, Intent::Group(Verb::Create));
    }

    mod late_message {
        include!("../tests/support/late_message.rs");
    }

    mod mailbox_page {
        include!("../tests/support/mailbox_page.rs");
    }

    /// 1642, the mailbox page names the mailbox: the gear and Mailbox open
    /// the core's page for the fixture's mailbox on miauhaus (the golden,
    /// light, 900 x 700); its "Copy" puts the address on the clipboard and
    /// says so; "Show my code" opens the profile.
    #[test]
    fn the_mailbox_page_names_the_mailbox_and_copies_its_address() {
        use loquax::ui::settings::Section;
        let (w, h) = (900, 700);
        let mut model = chat_model();
        mailbox_page::mailbox_told(&mut model, true);
        let (mut app, mut rx) = chat_window();
        app.apply(Message::Seen(Box::new(seen_of(&model))));
        app.window = Size::new(w as f32, h as f32);
        let _ = app.update(Message::Settings);
        let _ = app.update(Message::Intent(Intent::Settings(Some(Section::Mailbox))));
        assert_eq!(app.settings_at, Some(Section::Mailbox));
        let page = app.seen.mailbox.clone().expect("the page");
        assert_eq!(page.groups.len(), 3, "which, how, who");
        map::tests::same_as("gui-settings-mailbox-light.png", &shoot(&app, w, h), w, h);
        let address = loquax::address::to_hex(&mailbox_page::MAILBOX_ADDRESS);
        let _ = app.update(Message::CopyText(address));
        assert_eq!(
            app.seen.notice.as_deref(),
            Some("The mailbox's address is on the clipboard")
        );
        let _ = app.update(Message::Intent(Intent::Profile));
        assert_eq!(told(&mut rx), [UiEvent::Profile { open: true }]);
    }

    /// 1505, a late message sits at its sent time: the window draws Anna's
    /// chat of the shared fixture, the message she wrote last night at
    /// 22:10 that came today at 14:05 second under last night's day, the
    /// `new` rule above it, today's two below; pixel for pixel the golden,
    /// light, 900 x 560.
    #[test]
    fn a_late_message_is_the_windows_golden() {
        let mut model = crate::shell::Model::new(&[]);
        model.address = Some(OWN);
        late_message::late_message(&mut model, 1_799_971_200, 3600);
        model.toasts.clear();
        let (w, h) = (900, 560);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(Frames::new(loquax::picture::Maps::default()), Some(tx));
        app.window = Size::new(w as f32, h as f32);
        app.apply(Message::Seen(Box::new(seen_of(&model))));
        let bodies: Vec<String> = app
            .scene
            .chat
            .as_ref()
            .expect("the chat")
            .groups
            .iter()
            .flat_map(|g| &g.bubbles)
            .map(|b| b.bubble.body.clone())
            .collect();
        assert_eq!(
            bodies,
            [
                "Gute Nacht",
                "Kommst du morgen mit?",
                "Bis gleich am Bahnhof",
                "Bin unterwegs"
            ]
        );
        map::tests::same_as("gui-late-message.png", &shoot(&app, w, h), w, h);
    }

    mod group_ticks {
        include!("../tests/support/group_ticks.rs");
    }

    /// 1466, the group's ticks tell the truth too: the window draws the
    /// group's chat of `scene-group-ticks.json`, our line every member's
    /// device has with two ticks and the one Ben's device has not said yet
    /// with one; pixel for pixel the golden, light, 900 x 560.
    #[test]
    fn a_groups_one_and_two_ticks_are_the_windows_golden() {
        let mut model = crate::shell::Model::new(&[]);
        model.address = Some(OWN);
        group_ticks::group_ticks(&mut model, 1_800_000_000, 3600);
        update(
            &mut model,
            UiEvent::Open {
                peer: group_ticks::TICKS_GROUP,
            },
        );
        let (w, h) = (900, 560);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(Frames::new(loquax::picture::Maps::default()), Some(tx));
        app.window = Size::new(w as f32, h as f32);
        app.apply(Message::Seen(Box::new(seen_of(&model))));
        let marks: Vec<_> = app
            .scene
            .chat
            .as_ref()
            .expect("the chat")
            .groups
            .iter()
            .flat_map(|g| &g.bubbles)
            .filter_map(|b| b.mark.as_ref().map(|m| m.mark))
            .collect();
        assert_eq!(
            marks,
            [
                loquax::ui::chat::Mark::Delivered,
                loquax::ui::chat::Mark::Arrived
            ]
        );
        map::tests::same_as("gui-group-ticks.png", &shoot(&app, w, h), w, h);
    }

    mod relay_group {
        include!("../tests/support/relay_group.rs");
    }

    /// 1482, a relayed post shows its writer: the window draws the relay's
    /// chat of `scene-relay-group.json`, each post under its writer without
    /// the bot's prefix, the bar naming it a group via a relay; pixel for
    /// pixel the golden, light, 900 x 560.
    #[test]
    fn a_relayed_group_is_the_windows_golden() {
        let mut model = crate::shell::Model::new(&[]);
        model.address = Some(OWN);
        relay_group::relay_group(&mut model, 1_800_000_000, 3600);
        update(
            &mut model,
            UiEvent::Open {
                peer: relay_group::RELAY,
            },
        );
        model.toasts.clear();
        let (w, h) = (900, 560);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(Frames::new(loquax::picture::Maps::default()), Some(tx));
        app.window = Size::new(w as f32, h as f32);
        app.apply(Message::Seen(Box::new(seen_of(&model))));
        let writers: Vec<_> = app
            .scene
            .chat
            .as_ref()
            .expect("the chat")
            .groups
            .iter()
            .filter(|g| g.named)
            .map(|g| g.sender.resolve(Lang::En))
            .collect();
        assert_eq!(writers, ["Anna", "Ben", "Anna"]);
        map::tests::same_as("gui-relay-group.png", &shoot(&app, w, h), w, h);
    }

    mod reply_scene {
        include!("../tests/support/reply_scene.rs");
    }

    /// The window over the shared reply fixture's model, light, `w` x `h`.
    fn reply_window(model: &crate::shell::Model, w: u32, h: u32) -> App {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(Frames::new(loquax::picture::Maps::default()), Some(tx));
        app.window = Size::new(w as f32, h as f32);
        app.apply(Message::Seen(Box::new(seen_of(model))));
        app
    }

    /// Anna's chat of `reply_scene.rs` with the shared clock: today's
    /// midnight of a fixed day, an hour east of UTC, as the window keeps
    /// its scenes (whole).
    fn reply_model() -> crate::shell::Model {
        let mut model = crate::shell::Model::new(&[]);
        update(&mut model, UiEvent::Viewing(loquax::ui::Viewing::ALL));
        model.address = Some(OWN);
        reply_scene::reply_scene(&mut model, 1_800_000_000 - 1_800_000_000 % 86_400, 3600);
        model.toasts.clear();
        model
    }

    /// 1451, the walk's blind spots: a reply, a reaction and a photo in the
    /// window. Anna's chat of the shared fixture: her question, her photo,
    /// our reply "Ja, um zehn am Brunnen" quoting her question with her
    /// thumbs up as a chip under it, and her last line; pixel for pixel the
    /// golden, light, 900 x 560.
    #[test]
    fn a_reply_with_its_reaction_is_the_windows_golden() {
        let (w, h) = (900, 560);
        let model = reply_model();
        let app = reply_window(&model, w, h);
        let chat = app.scene.chat.as_ref().expect("Anna's chat");
        assert_eq!(chat.peer, reply_scene::REPLY_ANNA);
        let ours = chat
            .groups
            .iter()
            .flat_map(|g| &g.bubbles)
            .find(|b| b.bubble.id == Some(reply_scene::REPLY_OURS))
            .expect("our reply");
        assert_eq!(ours.bubble.body, "Ja, um zehn am Brunnen");
        assert_eq!(
            ours.bubble.quote.as_ref().map(|q| q.text.as_str()),
            Some("Kommst du morgen mit zum Markt?")
        );
        let chips: Vec<_> = ours
            .bubble
            .chips
            .iter()
            .map(|c| (c.content.as_str(), c.count, c.ours))
            .collect();
        assert_eq!(chips, [("\u{1F44D}", 1, false)]);
        assert!(
            chat.groups
                .iter()
                .flat_map(|g| &g.bubbles)
                .any(|b| b.bubble.id == Some(reply_scene::REPLY_PHOTO) && b.bubble.photo.is_some()),
            "her photo"
        );
        // Control: nothing is being written yet.
        assert_eq!(app.reply, None);
        assert_eq!(app.compose.text().trim(), "");
        map::tests::same_as("gui-reply-sent.png", &shoot(&app, w, h), w, h);
    }

    /// 1451: the same chat while a second reply is written, to Anna's last
    /// line. The window takes the draft and the reply back from the scene
    /// as when the reader's Reply had started it: the reply bar quoting
    /// "Super, bis morgen" over the field holding "Ich bring Kaffee mit";
    /// pixel for pixel the golden, light, 900 x 560.
    #[test]
    fn a_reply_being_written_is_the_windows_golden() {
        let (w, h) = (900, 560);
        let mut model = reply_model();
        reply_scene::reply_composing(&mut model);
        let app = reply_window(&model, w, h);
        assert_eq!(
            app.reply,
            Some((reply_scene::REPLY_LAST, "Super, bis morgen".to_string()))
        );
        assert_eq!(
            app.compose.text().trim_end_matches('\n'),
            reply_scene::REPLY_DRAFT
        );
        // The sent reply keeps its quote and its chip above the bar.
        let ours = app
            .scene
            .chat
            .iter()
            .flat_map(|c| &c.groups)
            .flat_map(|g| &g.bubbles)
            .find(|b| b.bubble.id == Some(reply_scene::REPLY_OURS))
            .expect("our reply");
        assert!(ours.bubble.quote.is_some() && !ours.bubble.chips.is_empty());
        map::tests::same_as("gui-reply-compose.png", &shoot(&app, w, h), w, h);
    }

    /// The model at the window's very first start (1451): an address and
    /// its code, the profile's facts as the shell gives a messenger without
    /// a mailbox (`shell::profile_facts` with no client: `Mailbox::Unknown`
    /// and this one device), no conversation.
    fn first_start_model() -> crate::shell::Model {
        use loquax::ui::profile::{Device, Mailbox, ProfileFacts};
        let mut model = crate::shell::Model::new(&[]);
        update(&mut model, UiEvent::Viewing(loquax::ui::Viewing::ALL));
        model.address = Some(OWN);
        update(&mut model, UiEvent::Clock(at(29, 9, 30)));
        let private: [u8; 64] = core::array::from_fn(|i| i as u8 + 9);
        let key = leviculum_core::Identity::from_private_key_bytes(&private)
            .expect("a key")
            .public_key_bytes();
        let code = loquax::lxma::encode(&key).expect("a code");
        model.own_code = Some(loquax::ui::OwnCode::of(
            code.clone(),
            loquax::home::qr::verbatim_modules(&code).ok(),
        ));
        model.profile = ProfileFacts {
            name: "Mia Sander".to_string(),
            mailbox: Mailbox::Unknown,
            devices: vec![Device {
                name: String::new(),
                this: true,
            }],
            ..Default::default()
        };
        model.toasts.clear();
        model
    }

    /// 1451, the window at its very first start, before any mailbox: no
    /// conversation in the list, no chat open; pixel for pixel the golden.
    /// The avatar opens the profile, which says the mailbox is not set up
    /// yet, the golden too; both light, 900 x 560.
    #[test]
    fn the_first_start_and_its_profile_are_the_windows_goldens() {
        let (w, h) = (900, 560);
        let mut model = first_start_model();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(Frames::new(loquax::picture::Maps::default()), Some(tx));
        app.window = Size::new(w as f32, h as f32);
        app.apply(Message::Seen(Box::new(seen_of(&model))));
        assert!(model.conversations.is_empty());
        assert!(app.listed().is_empty(), "no conversation");
        assert!(app.scene.chat.is_none(), "no chat open");
        assert!(app.scene.profile.is_none(), "control: closed");
        map::tests::same_as("gui-first-start.png", &shoot(&app, w, h), w, h);
        let open = app
            .scene
            .action(NodeId::Me)
            .expect("the avatar does something")
            .intent
            .clone();
        assert_eq!(open, Intent::Profile);
        let _ = app.update(Message::Intent(open));
        for event in told(&mut rx) {
            update(&mut model, event);
        }
        app.apply(Message::Seen(Box::new(seen_of(&model))));
        let profile = app.scene.profile.as_ref().expect("the profile");
        assert_eq!(profile.mailbox.resolve(Lang::En), "Mailbox: not set up yet");
        assert_eq!(profile.state, None);
        assert_eq!(profile.devices.len(), 1);
        map::tests::same_as("gui-first-start-profile.png", &shoot(&app, w, h), w, h);
    }

    /// 1451, the map in the window with two peers' pins: Anna and Ben
    /// shared their positions in Florence's centre, inside the committed
    /// region, five and eight minutes ago, the window shows the map screen over the
    /// committed Florence region, Anna selected, her card marked in the
    /// list and the view centred on her; pixel for pixel the golden,
    /// light, 900 x 560.
    #[test]
    fn two_pins_with_one_selected_are_the_windows_map_golden() {
        use leviculum_lxmf::telemetry::Location;
        const BEN: [u8; 16] = [0xbe; 16];
        let (w, h) = (900, 560);
        let mut model = crate::shell::Model::new(&[]);
        update(&mut model, UiEvent::Viewing(loquax::ui::Viewing::ALL));
        model.address = Some(OWN);
        model.names.remember(ANNA, "Anna");
        model.names.remember(BEN, "Ben");
        let now = at(29, 9, 30);
        update(&mut model, UiEvent::Clock(now));
        let mut report = |peer: [u8; 16], id: u8, lat: i32, lon: i32, ago: i64| {
            let fix = Location {
                latitude_e6: lat,
                longitude_e6: lon,
                altitude_e2: 5_000,
                speed_e2: 0,
                bearing_e2: 0,
                accuracy_e2: 500,
                last_update: now.unix_secs - ago,
            };
            update(
                &mut model,
                UiEvent::Engine(OutboxEvent::Received {
                    sent_at: now.unix_secs - ago,
                    message_id: [id; 32],
                    source: peer,
                    title: Vec::new(),
                    body: Vec::new(),
                    fields: loquax::ui::position_fields(fix),
                    verified: true,
                }),
            );
        };
        report(ANNA, 1, 43_775_630, 11_260_220, 300);
        report(BEN, 2, 43_774_030, 11_255_720, 480);
        model.toasts.clear();
        let frames = map::tests::florence();
        let maps = frames.painter().epoch();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(frames, Some(tx));
        app.window = Size::new(w as f32, h as f32);
        let mut seen = seen_of(&model);
        seen.maps = maps;
        app.apply(Message::Seen(Box::new(seen)));
        let _ = app.update(Message::Intent(Intent::ShowMap { peer: None }));
        assert_eq!(app.screen, Screen::Map);
        let _ = told(&mut rx);
        // The map's canvas in a 900 x 560 window: right of the rail and
        // the list, above the bar.
        app.apply(Message::MapSize(Size::new(568.0, 539.0)));
        app.apply(Message::Select(ANNA));
        let (shown, _) = run(&mut app);
        assert!(shown.is_some(), "a frame of Florence at rest");
        let labels: Vec<_> = app.seen.pins.iter().map(|p| p.label.as_str()).collect();
        assert_eq!(labels.len(), 2, "{labels:?}");
        assert!(labels.iter().any(|l| l.starts_with("Anna")), "{labels:?}");
        assert!(labels.iter().any(|l| l.starts_with("Ben")), "{labels:?}");
        assert_eq!(app.selected, Some(ANNA));
        let anna = app
            .seen
            .pins
            .iter()
            .find(|p| p.peer == ANNA)
            .expect("Anna's pin");
        assert_eq!(app.view.centre, anna.at, "the view is on Anna");
        map::tests::same_as("gui-map-pins.png", &shoot(&app, w, h), w, h);
    }

    /// `pcm` as the microphone's levels for Anna's recording, a window
    /// every 50 ms from `from` on; the time after the last.
    fn levels_of(model: &mut crate::shell::Model, pcm: &[i16], from: u64) -> u64 {
        use loquax::ui::recording::{rms, Input, WINDOW_MS};
        let mut now = from;
        for chunk in pcm.chunks((loquax::voice::RATE / (1000 / WINDOW_MS)) as usize) {
            now += u64::from(WINDOW_MS);
            update(
                model,
                UiEvent::Record {
                    peer: ANNA,
                    input: Input::Level {
                        now,
                        rms: rms(chunk),
                    },
                },
            );
        }
        now
    }

    fn rec(model: &mut crate::shell::Model, input: loquax::ui::recording::Input) {
        update(model, UiEvent::Record { peer: ANNA, input });
    }

    /// The recording bar in the window (1276), pixel for pixel the
    /// goldens, light, 900 x 560: held (the red dot, `REC 00:03`, the
    /// live waveform, the cancel hint followed 40 px left, the grown red
    /// microphone with the open lock chip risen above it), locked after a
    /// voice message already went (its bubble draws the recording's own
    /// waveform; the bar has the bin, the pause and the big Send under
    /// the closed lock), and paused after three silent seconds (the
    /// hollow dot, the play, the bin, resume, and the amber line with its
    /// Allow). Each picture is the same when taken again.
    #[test]
    fn the_recording_bar_is_the_golden_held_locked_and_paused() {
        use loquax::ui::recording::{Input, Phase, Verb};
        let speech = loquax::voice::test_speech(loquax::voice::RATE, 7_400);
        let (w, h) = (900, 560);
        let shot = |model: &crate::shell::Model| {
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
            let mut app = App::new(Frames::new(loquax::picture::Maps::default()), Some(tx));
            app.apply(Message::Seen(Box::new(seen_of(model))));
            app.window = Size::new(w as f32, h as f32);
            let first = shoot(&app, w, h);
            assert_eq!(shoot(&app, w, h), first, "a second picture is the same");
            first
        };

        let mut held = chat_model();
        rec(&mut held, Input::Press { now: 1_000 });
        levels_of(&mut held, &speech[..48_000], 1_000);
        rec(&mut held, Input::Slide { dx: -40, dy: -30 });
        assert!(matches!(held.recording.phase, Phase::Held { .. }));

        let mut locked = chat_model();
        rec(&mut locked, Input::Press { now: 1_000 });
        let end = levels_of(&mut locked, &speech[..48_000], 1_000);
        rec(&mut locked, Input::Release { now: end });
        assert_eq!(locked.recording.phase, Phase::Sent);
        let data = loquax::voice::encode(&speech[..48_000], loquax::voice::RATE).expect("coded");
        update(&mut locked, UiEvent::SendVoice { peer: ANNA, data });
        rec(&mut locked, Input::Press { now: 20_000 });
        rec(&mut locked, Input::Verb(Verb::Lock));
        levels_of(&mut locked, &speech, 20_000);
        assert!(matches!(locked.recording.phase, Phase::Locked { .. }));

        let mut paused = chat_model();
        rec(&mut paused, Input::Press { now: 1_000 });
        rec(&mut paused, Input::Verb(Verb::Lock));
        levels_of(&mut paused, &vec![0; 48_000], 1_000);
        rec(&mut paused, Input::Verb(Verb::Pause));
        assert_eq!(paused.recording.phase, Phase::Paused);

        for (name, model) in [
            ("gui-recording-held.png", &held),
            ("gui-recording-locked.png", &locked),
            ("gui-recording-paused.png", &paused),
        ] {
            map::tests::same_as(name, &shot(model), w, h);
        }
    }

    /// The mouse on the window's microphone (1276): down is the press,
    /// the moves are slides from where the pointer was first seen, up is
    /// the release; up over the lock chip locks instead; the bar's
    /// buttons are the model's verbs. Every one goes to the shell, which
    /// times it.
    #[test]
    fn the_mouse_holds_slides_locks_and_releases_the_microphone() {
        use loquax::ui::recording::{Input, Verb};
        let (mut app, mut rx) = chat_window();
        let mut sent = || {
            let mut out = Vec::new();
            while let Ok(want) = rx.try_recv() {
                if let Want::Record { peer, input } = want {
                    assert_eq!(peer, ANNA);
                    out.push(input);
                }
            }
            out
        };
        let _ = app.update(Message::RecPress);
        let _ = app.update(Message::RecCursor(Point::new(800.0, 500.0)));
        let _ = app.update(Message::RecCursor(Point::new(700.0, 480.0)));
        let _ = app.update(Message::RecRelease);
        assert_eq!(
            sent(),
            [
                Input::Press { now: 0 },
                Input::Slide { dx: 0, dy: 0 },
                Input::Slide { dx: -100, dy: -20 },
                Input::Release { now: 0 },
            ]
        );
        // Released again: nothing, it is no longer held.
        let _ = app.update(Message::RecRelease);
        let _ = app.update(Message::RecCursor(Point::new(1.0, 1.0)));
        assert!(sent().is_empty());
        // Up over the chip: the lock.
        let _ = app.update(Message::RecPress);
        let _ = app.update(Message::RecOverLock(true));
        let _ = app.update(Message::RecRelease);
        assert_eq!(sent(), [Input::Press { now: 0 }, Input::Verb(Verb::Lock)]);
        // A button of the bar.
        let _ = app.update(Message::Intent(Intent::Recording {
            peer: ANNA,
            verb: Verb::Pause,
        }));
        assert_eq!(sent(), [Input::Verb(Verb::Pause)]);
    }

    /// The map's pane keeps its size whatever its words say (1441): the
    /// peers' column is the list's width with a short name and a long
    /// one, and the map beside it the same rectangle with a long notice
    /// and a long credit in its bar, which before wrapped to a second line
    /// and took the map 17 pixels of height while a pan loaded its tiles,
    /// the window Lew measured and a small one. The control: the column
    /// is where the test looks for it, the list's width.
    #[test]
    fn the_maps_pane_keeps_its_size_whatever_its_words() {
        use iced::advanced::Layout;
        let long = "a long notice that goes on past the bar's width: Loading the region of Frankfurt am Main and its surroundings, 45 of 120 tiles, about two minutes left";
        for (w, h) in [(1280u32, 1190u32), (900, 560)] {
            let mut panes = Vec::new();
            for (label, notice) in [
                ("al", None),
                (
                    "a peer whose name is far wider than the column of peers",
                    Some(long),
                ),
            ] {
                let mut app =
                    window_over_florence(vec![pin(1, label, 43.7752, 11.2640)], 14 * MILLI);
                app.window = Size::new(w as f32, h as f32);
                app.seen.notice = notice.map(str::to_string);
                app.seen.loading = notice.map(str::to_string);
                let _ = run(&mut app);
                let node =
                    leviculum_ui_iced::headless::layout(app.view(), w, h).expect("a renderer");
                // The rail, then the map screen: the column and the map
                // over its bar.
                let screen = Layout::new(&node)
                    .children()
                    .nth(1)
                    .expect("the map screen");
                let mut parts = screen.children();
                let column = parts.next().expect("the column").bounds();
                let map = parts
                    .next()
                    .expect("the map")
                    .children()
                    .next()
                    .expect("the map")
                    .bounds();
                assert_eq!(column.width, LIST_WIDTH, "{label}");
                panes.push((column.x, column.width, map));
            }
            assert_eq!(panes[0], panes[1], "{w} x {h}");
        }
    }

    /// Never black (1441): a frame the view moved half off the canvas, as
    /// a drag past its margin leaves it until the next frame lands, shows
    /// the map's ground in the dark theme where it does not reach, the
    /// land a frame is cleared to, and not the window's ground; pixel for
    /// pixel the golden. The control: the half the frame covers is the
    /// map, not the ground.
    #[test]
    fn a_map_half_off_its_frame_shows_its_ground_not_black() {
        let (w, h) = (480u32, 320u32);
        let mut app = window_over_florence(Vec::new(), 16 * MILLI);
        app.seen.style = Style::SCREEN.in_theme(leviculum_mapfill::style::MapTheme::Dark);
        let _ = run(&mut app);
        assert!(app.shown.is_some(), "a frame at rest");
        app.view.pan(i32::try_from(w / 2).expect("small"), 0);
        let picture: Element<'_, Message> = Canvas::new(PictureCanvas { app: &app })
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        let rgb = leviculum_ui_iced::headless::render(
            picture,
            &Theme::Dark,
            Color::WHITE,
            Color::BLACK,
            w,
            h,
        )
        .expect("a renderer");
        let [r, g, b] = leviculum_mapfill::frame::rgb888(
            leviculum_mapfill::style::MapTheme::Dark
                .palette()
                .background,
        );
        let px = |x: u32, y: u32| {
            let i = ((y * w + x) * 3) as usize;
            [rgb[i], rgb[i + 1], rgb[i + 2]]
        };
        let black = rgb.chunks(3).filter(|p| p == &[0, 0, 0]).count();
        assert_eq!(black, 0, "black pixels");
        // The frame's margin is 60 pixels: 180 of the right are uncovered.
        for y in (0..h).step_by(40) {
            assert_eq!(px(w - 5, y), [r, g, b], "uncovered at {y}");
        }
        let map = (0..w / 3)
            .flat_map(|x| (0..h).map(move |y| (x, y)))
            .filter(|&(x, y)| px(x, y) != [r, g, b])
            .count();
        assert!(map > 1000, "the covered half is the map: {map} pixels");
        map::tests::same_as("gui-map-uncovered-dark.png", &rgb, w, h);
    }

    /// The tracks' time window is in the map's menu, not on the map
    /// (1419): the bar has no strip, its title is "Map" with the default;
    /// More opens the menu with the default first and marked, a choice
    /// tells the model and closes it, and a window that is not the default
    /// puts its word after the title; Escape closes an open menu before
    /// the map. Pixel for pixel the goldens: the map, the menu open, the
    /// title with "This week".
    #[test]
    fn the_time_window_is_in_the_maps_menu() {
        use loquax::ui::tracks::TrackWindow;
        let (w, h) = (900, 560);
        let (mut app, mut rx) = chat_window();
        app.window = Size::new(w as f32, h as f32);
        let _ = app.update(Message::Intent(Intent::ShowMap { peer: None }));
        assert_eq!(app.screen, Screen::Map);
        let _ = told(&mut rx);
        assert!(!app.map_menu);
        map::tests::same_as("gui-map-window.png", &shoot(&app, w, h), w, h);
        let _ = app.update(Message::MapMenu);
        assert!(app.map_menu);
        map::tests::same_as("gui-map-window-menu.png", &shoot(&app, w, h), w, h);
        let _ = app.update(Message::TrackWindow(TrackWindow::Week));
        assert!(!app.map_menu, "a choice closes the menu");
        assert_eq!(told(&mut rx), [UiEvent::TrackWindow(TrackWindow::Week)]);
        // The model's answer, as the shell's next Seen brings it.
        app.seen.track_window = TrackWindow::Week;
        map::tests::same_as("gui-map-window-week.png", &shoot(&app, w, h), w, h);
        let _ = app.update(Message::MapMenu);
        let _ = app.update(Message::Intent(Intent::Back));
        assert!(!app.map_menu);
        assert_eq!(app.screen, Screen::Map, "Back closed the menu only");
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

    /// 1577, 1599: Advanced holds the public rooms' hub (the golden): the
    /// closest ten hubs by name and how far, "More…", the mark on none; a
    /// press on one tells the model, the model's answer moves the mark,
    /// "More…" tells the model, and the field's address goes to the model
    /// as typed. No hash on the page.
    #[test]
    fn advanced_chooses_the_public_rooms_hub() {
        use loquax::ui::rooms::{HubChoice, HubPick, RoomsEvent};
        use loquax::ui::settings::Section;
        let (w, h) = (900, 700);
        let (mut app, mut rx) = chat_window();
        app.window = Size::new(w as f32, h as f32);
        let _ = app.update(Message::Settings);
        let _ = app.update(Message::Intent(Intent::Settings(Some(Section::Advanced))));
        assert_eq!(app.settings_at, Some(Section::Advanced));
        // The line, ten hubs, More, none, paste, the note.
        assert_eq!(app.seen.hubs.len(), 15, "{:?}", app.seen.hubs);
        map::tests::same_as("gui-settings-advanced-light.png", &shoot(&app, w, h), w, h);
        let _ = app.update(Message::Intent(Intent::RoomsHub(HubPick::More)));
        assert_eq!(told(&mut rx), [UiEvent::Rooms(RoomsEvent::More)]);
        let community = loquax::ui::hubs::shipped()[0].hash;
        let _ = app.update(Message::Intent(Intent::RoomsHub(HubPick::Hub(community))));
        assert_eq!(
            told(&mut rx),
            [UiEvent::Rooms(RoomsEvent::Hub(HubChoice::Hub(community)))]
        );
        let mut model = chat_model();
        update(
            &mut model,
            UiEvent::Rooms(RoomsEvent::Hub(HubChoice::Hub(community))),
        );
        let _ = app.update(Message::Seen(Box::new(seen_of(&model))));
        let chosen: Vec<bool> = app
            .seen
            .hubs
            .iter()
            .map(|r| {
                r.value
                    .as_ref()
                    .is_some_and(|v| v.resolve(app.lang).ends_with('\u{2713}'))
            })
            .collect();
        assert_eq!(
            chosen[..3],
            [false, true, false],
            "the line names it, the mark is on the first hub"
        );
        let _ = app.update(Message::HubTyped(
            " 07070707070707070707070707070707 ".into(),
        ));
        let _ = app.update(Message::HubTake);
        assert_eq!(
            told(&mut rx),
            [UiEvent::Rooms(RoomsEvent::Hub(HubChoice::Address(
                " 07070707070707070707070707070707 ".into()
            )))]
        );
        assert!(app.hub_field.is_empty(), "the field empties");
        let _ = app.update(Message::HubTake);
        assert!(told(&mut rx).is_empty(), "an empty field tells nothing");
    }

    /// The settings page (1233, one structure since 1450): the gear opens
    /// the list of the nine sections (the golden), Privacy holds read
    /// receipts (its golden): the row shows the model's switch, a toggle
    /// tells the model the other value, the model's answer turns the row;
    /// Back goes to the list, then to the chats. Appearance holds light
    /// or dark, which the bar no longer does.
    #[test]
    fn the_settings_page_switches_read_receipts_in_the_model() {
        use loquax::ui::settings::Section;
        let (w, h) = (900, 560);
        let (mut app, mut rx) = chat_window();
        app.window = Size::new(w as f32, h as f32);
        let _ = app.update(Message::Settings);
        assert_eq!(app.screen, Screen::Settings);
        assert_eq!(app.settings_at, None, "the list");
        map::tests::same_as("gui-settings-light.png", &shoot(&app, w, h), w, h);
        let _ = app.update(Message::Intent(Intent::Settings(Some(Section::Appearance))));
        assert_eq!(app.settings_at, Some(Section::Appearance));
        assert!(!app.dark());
        let _ = app.update(Message::Intent(Intent::Theme));
        assert!(app.dark(), "Appearance's switch");
        let _ = app.update(Message::Intent(Intent::Theme));
        let _ = app.update(Message::Intent(Intent::Settings(Some(Section::Privacy))));
        assert_eq!(app.settings_at, Some(Section::Privacy));
        assert!(app.seen.receipts, "on by default");
        map::tests::same_as("gui-settings-privacy-light.png", &shoot(&app, w, h), w, h);
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
        assert_eq!(
            (app.screen, app.settings_at),
            (Screen::Settings, None),
            "a section goes back to the list"
        );
        // Profile opens the avatar's layer over the list.
        let _ = app.update(Message::Intent(Intent::Settings(Some(Section::Profile))));
        assert_eq!(told(&mut rx), [UiEvent::Profile { open: true }]);
        assert_eq!(app.settings_at, None);
        let _ = app.update(Message::Intent(Intent::Back));
        assert_eq!(app.screen, Screen::Chats);
    }

    /// 1290: the settings page's Maps rows delete every region after
    /// asking once: the label's total is the three regions', Back takes
    /// the question away and keeps the page, a yes deletes the files,
    /// which the painter no longer draws, and an empty list asks nothing.
    #[test]
    fn the_settings_page_deletes_all_maps_after_asking_once() {
        let dir = tempfile::tempdir().expect("maps dir");
        let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../leviculum-map/fixtures/firenze.lmr");
        for name in ["firenze.lmr", "auto-a.lmr", "auto-b.lmr"] {
            std::fs::copy(&fixture, dir.path().join(name)).expect("a region");
        }
        let len = std::fs::metadata(&fixture).expect("its size").len();
        let maps = loquax::picture::Maps::new(Some(dir.path().to_path_buf()));
        let mut app = App::new(Frames::new(maps), None);
        app.seen.autoload = true;
        assert_eq!(app.setup.regions.len(), 3);
        let _ = app.update(Message::Settings);
        let _ = app.update(Message::Intent(Intent::Settings(Some(
            loquax::ui::settings::Section::Maps,
        ))));
        assert_eq!(app.maps_usage, (3, 3 * len));
        assert!(!app.maps_asked);

        let _ = app.update(Message::Intent(Intent::DeleteAllRegions {
            confirmed: false,
        }));
        assert!(app.maps_asked);
        let _ = app.update(Message::Intent(Intent::Back));
        assert!(!app.maps_asked, "Back takes the question away");
        assert_eq!(app.screen, Screen::Settings, "and keeps the page");
        assert!(dir.path().join("firenze.lmr").is_file());

        let _ = app.update(Message::Intent(Intent::DeleteAllRegions {
            confirmed: false,
        }));
        let _ = app.update(Message::Intent(Intent::DeleteAllRegions {
            confirmed: true,
        }));
        assert!(!app.maps_asked);
        assert_eq!(app.maps_usage, (0, 0));
        assert!(std::fs::read_dir(dir.path())
            .expect("the dir")
            .next()
            .is_none());
        assert!(app.setup.regions.is_empty(), "the painter drew them last");
        assert_eq!(app.screen, Screen::Settings);

        let _ = app.update(Message::Intent(Intent::DeleteAllRegions {
            confirmed: false,
        }));
        assert!(!app.maps_asked, "nothing to ask about");
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

    /// A PNG of `w` by `h` pixels, a gradient, for a page's picture.
    fn png_of(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        let mut e = png::Encoder::new(&mut out, w, h);
        e.set_color(png::ColorType::Rgb);
        e.set_depth(png::BitDepth::Eight);
        let mut writer = e.write_header().expect("header");
        let data: Vec<u8> = (0..h)
            .flat_map(|y| (0..w).flat_map(move |x| [(x * 255 / w) as u8, (y * 255 / h) as u8, 160]))
            .collect();
        writer.write_image_data(&data).expect("data");
        writer.finish().expect("finish");
        out
    }

    /// 1369: the Browse tab in the window is lnomad's window view beside
    /// the rail, the browser the model hosts with the pictures the shell's
    /// half of lnomad's picture code holds. The window tells the model its
    /// light or dark and the page's cells; the list it opens on and a page
    /// with a picture are the goldens; a press on the page, the wheel and
    /// the pointer go to the model as lnomad's window folds them; leaving
    /// for the chats tells the model, and a picture of the model from
    /// before does not pull the window back (the control: one from after
    /// leaves it in the chats).
    /// 1379: the Rooms tab in the window: the tab press goes to the
    /// model, the model's Rooms bring the window there, the open room is
    /// drawn from the scene's Rooms node (the golden), the input's text and
    /// Enter and a room's press go to the model; leaving for the chats is
    /// told, and a picture from before does not pull the window back (the
    /// control: one from after leaves it in the chats).
    #[test]
    fn the_rooms_tab_is_the_scenes_rooms_beside_the_rail() {
        use leviculum_rrc::LineKind;
        use loquax::ui::rooms::RoomsEvent;
        let (w, h) = (1100u32, 640u32);
        let mut model = chat_model();
        model.rooms.enabled = true;
        model.rooms.hub = Some([0x07; 16]);
        let seen = |model: &crate::shell::Model| Box::new(seen_of(model));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(Frames::new(loquax::picture::Maps::default()), Some(tx));
        app.window = Size::new(w as f32, h as f32);
        let _ = app.update(Message::Seen(seen(&model)));
        assert_eq!(app.screen, Screen::Chats);
        let _ = app.update(Message::Intent(Intent::Tab(TabId::Rooms)));
        assert_eq!(told(&mut rx), vec![UiEvent::Tab(TabId::Rooms)]);
        update(&mut model, UiEvent::Tab(TabId::Rooms));
        for event in [
            RoomsEvent::Welcomed {
                name: Some("periculum".into()),
                body_limit: 350,
            },
            RoomsEvent::Joined {
                room: "#lab".into(),
                nick: None,
            },
            RoomsEvent::Joined {
                room: "#lab".into(),
                nick: Some("bob".into()),
            },
            RoomsEvent::Line {
                room: "#lab".into(),
                kind: LineKind::Msg,
                text: "a line, twenty-five bytes".into(),
                nick: Some("bob".into()),
                ours: false,
                ts_ms: 1_793_702_757_876,
            },
            RoomsEvent::Line {
                room: "#lab".into(),
                kind: LineKind::Msg,
                text: "hello bob".into(),
                nick: Some("lew".into()),
                ours: true,
                ts_ms: 1_793_702_759_000,
            },
        ] {
            update(&mut model, UiEvent::Rooms(event));
        }
        let _ = app.update(Message::Seen(seen(&model)));
        assert_eq!(app.screen, Screen::Rooms);
        let room = shoot(&app, w, h);
        map::tests::same_as("gui-rooms-room.png", &room, w, h);

        let _ = app.update(Message::Rooms(RoomsEvent::Typed("hi".into())));
        let _ = app.update(Message::Rooms(RoomsEvent::Submit));
        let _ = app.update(Message::Rooms(RoomsEvent::Show(Some(0))));
        // 1450: the room's menu is the scene's, "Leave room" among it.
        let menu = app
            .seen
            .scene
            .rooms
            .as_ref()
            .and_then(|r| r.open.as_ref())
            .map(|o| o.menu.clone())
            .expect("the open room's menu");
        let leave = menu
            .iter()
            .find(|v| v.event == RoomsEvent::Leave)
            .expect("Leave room");
        let _ = app.update(Message::Rooms(leave.event.clone()));
        assert_eq!(
            told(&mut rx),
            vec![
                UiEvent::Rooms(RoomsEvent::Typed("hi".into())),
                UiEvent::Rooms(RoomsEvent::Submit),
                UiEvent::Rooms(RoomsEvent::Show(Some(0))),
                UiEvent::Rooms(RoomsEvent::Leave),
            ]
        );

        // Leaving: told, and a stale picture keeps the window in the chats.
        let stale = seen(&model);
        let _ = app.update(Message::Intent(Intent::Tab(TabId::Chats)));
        assert_eq!(told(&mut rx), vec![UiEvent::Tab(TabId::Chats)]);
        assert_eq!(app.screen, Screen::Chats);
        let _ = app.update(Message::Seen(stale));
        assert_eq!(app.screen, Screen::Chats, "a picture from before");
        update(&mut model, UiEvent::Tab(TabId::Chats));
        let _ = app.update(Message::Seen(seen(&model)));
        assert_eq!(app.screen, Screen::Chats);
        assert!(app.seen.scene.rooms.is_none());
    }

    /// 1578: Explore beside the rail, one scroll for the pages and the
    /// public rooms, the golden: a bookmark, a page heard today and one
    /// older, the hubs. A press on a page is the model's verb, and the
    /// window follows the model into the browser and back to the list;
    /// Chats leaves it, and a picture from before does not bring it back.
    #[test]
    fn explore_is_one_scroll_beside_the_rail() {
        use lnomad::DiscoveredNode;
        use loquax::ui::browse::BrowseEvent;
        use loquax::ui::explore::ExploreEvent;
        const NOW: i64 = 1_800_000_000;
        let node = |byte: u8, name: &str, ago: i64, hops| DiscoveredNode {
            dest_hash: [byte; 16],
            name: Some(name.to_string()),
            first_seen: (NOW - ago) as u64,
            last_seen: (NOW - ago) as u64,
            hops: Some(hops),
        };
        let (w, h) = (1100u32, 640u32);
        let mut model = chat_model();
        model.browse.enabled = true;
        model.now = Some(loquax::when::WallTime {
            unix_secs: NOW,
            offset_secs: 0,
        });
        for found in [
            node(0x5a, "Marcel's node", 600, 2),
            node(0x7c, "Relay Süd", 2 * 86_400, 1),
            node(0x3d, "Bergwacht", 20 * 86_400, 4),
        ] {
            update(&mut model, UiEvent::Browse(BrowseEvent::Node(found)));
        }
        model.browse.model.bookmarks.add(lnomad::Bookmark {
            url: format!("{}:/page/index.mu", "7c".repeat(16)),
            title: "Relay Süd".to_string(),
        });
        // The browser rides in the picture as the shell sends it.
        let pictures: crate::browsing::Pictures = Arc::default();
        let seen = |model: &crate::shell::Model| {
            let mut seen = seen_of(model);
            seen.browse = model.browse.showing().then(|| Browsed {
                model: Arc::new(model.browse.model.clone()),
                scene: model.browse.model.scene(),
                pictures: Arc::clone(&pictures),
                epoch: 1,
            });
            Box::new(seen)
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(Frames::new(loquax::picture::Maps::default()), Some(tx));
        app.window = Size::new(w as f32, h as f32);
        let _ = app.update(Message::Seen(seen(&model)));
        let _ = app.update(Message::Intent(Intent::Tab(TabId::Explore)));
        assert_eq!(told(&mut rx), vec![UiEvent::Tab(TabId::Explore)]);
        update(&mut model, UiEvent::Tab(TabId::Explore));
        let _ = app.update(Message::Seen(seen(&model)));
        assert_eq!(app.screen, Screen::Explore);
        let list = shoot(&app, w, h);
        map::tests::same_as("gui-explore.png", &list, w, h);

        let _ = app.update(Message::Explore(ExploreEvent::OpenPage([0x5a; 16])));
        assert_eq!(
            told(&mut rx),
            vec![UiEvent::Explore(ExploreEvent::OpenPage([0x5a; 16]))]
        );
        update(
            &mut model,
            UiEvent::Explore(ExploreEvent::OpenPage([0x5a; 16])),
        );
        let _ = app.update(Message::Seen(seen(&model)));
        assert_eq!(app.screen, Screen::Browse, "the browser it opened");
        // The browser is told the window's theme and cells, as ever.
        let _ = told(&mut rx);
        // The browser's quit gives the list back.
        update(
            &mut model,
            UiEvent::Browse(BrowseEvent::Intent(lnomad::Intent::Quit)),
        );
        let _ = app.update(Message::Seen(seen(&model)));
        assert_eq!(app.screen, Screen::Explore);

        let stale = seen(&model);
        let _ = app.update(Message::Intent(Intent::Tab(TabId::Chats)));
        assert_eq!(told(&mut rx), vec![UiEvent::Tab(TabId::Chats)]);
        assert_eq!(app.screen, Screen::Chats);
        let _ = app.update(Message::Seen(stale));
        assert_eq!(app.screen, Screen::Chats, "a picture from before");
    }

    #[test]
    fn the_browse_tab_is_lnomads_window_beside_the_rail() {
        use lnomad::app::scene::NodeId as Node;
        use lnomad::DiscoveredNode;
        const MARCEL: [u8; 16] = [0x5a; 16];
        lnomad::gui::load_mono();
        let node = |dest_hash, name: &str, hops| DiscoveredNode {
            dest_hash,
            name: Some(name.to_string()),
            first_seen: 1_800_000_000,
            last_seen: 1_800_000_000,
            hops: Some(hops),
        };
        let (w, h) = (1100u32, 640u32);
        let mut model = chat_model();
        model.browse.enabled = true;
        model.browse.model.clock = Some(1_800_000_060);
        for found in [
            node(MARCEL, "Marcel's node", 2),
            node([0x7c; 16], "Relay Süd", 1),
        ] {
            update(&mut model, UiEvent::Browse(BrowseEvent::Node(found)));
        }
        let pictures: crate::browsing::Pictures = Arc::default();
        let seen = |model: &crate::shell::Model, epoch: u64| {
            let mut seen = seen_of(model);
            seen.browse = model.browse.showing().then(|| Browsed {
                model: Arc::new(model.browse.model.clone()),
                scene: model.browse.model.scene(),
                pictures: Arc::clone(&pictures),
                epoch,
            });
            Box::new(seen)
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(Frames::new(loquax::picture::Maps::default()), Some(tx));
        app.window = Size::new(w as f32, h as f32);
        let _ = app.update(Message::Seen(seen(&model, 0)));
        assert_eq!(app.screen, Screen::Chats);
        let _ = app.update(Message::Intent(Intent::Tab(TabId::Browse)));
        assert_eq!(told(&mut rx), vec![UiEvent::Tab(TabId::Browse)]);
        update(&mut model, UiEvent::Tab(TabId::Browse));
        let _ = app.update(Message::Seen(seen(&model, 0)));
        assert_eq!(app.screen, Screen::Browse);
        let (cols, rows) = app.browse_cells();
        let said = told(&mut rx);
        assert!(
            said.contains(&UiEvent::Browse(BrowseEvent::Resize { cols, rows })),
            "{said:?}"
        );
        for event in said {
            update(&mut model, event);
        }
        assert_eq!(model.browse.model.theme, lnomad::Theme::Light);
        let _ = app.update(Message::Seen(seen(&model, 0)));
        assert_eq!(told(&mut rx), vec![], "nothing more to say");
        let list = shoot(&app, w, h);
        map::tests::same_as("gui-browse-list.png", &list, w, h);

        // Marcel's row, pressed: the model's to fold.
        let row = lnomad::app::places(&model.browse.model)
            .iter()
            .position(
                |p| matches!(p, lnomad::app::Place::Node { dest_hash, .. } if *dest_hash == MARCEL),
            )
            .expect("Marcel's row");
        let press = lnomad::Intent::Activate(Node::Place(row));
        let _ = app.update(Message::Browse(lnomad::gui::Message::Intent(press.clone())));
        let said = told(&mut rx);
        assert_eq!(said, vec![UiEvent::Browse(BrowseEvent::Intent(press))]);
        for event in said {
            update(&mut model, event);
        }
        let micron = std::fs::read_to_string(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../lnomad/tests/fixtures/sample.mu"),
        )
        .expect("lnomad's sample page");
        let effects = update(
            &mut model,
            UiEvent::Browse(BrowseEvent::Loaded {
                micron: format!("{micron}\n`[map.png`:/file/map.png]\n\nUnder the picture."),
                title: "Marcel's node".to_string(),
            }),
        );
        // The shell's half: the picture asked for, its bytes taken by
        // lnomad's picture code, its event folded into the model.
        let requests = effects
            .into_iter()
            .find_map(|effect| match effect {
                loquax::ui::UiEffect::Browse(lnomad::Effect::FetchImages(requests)) => {
                    Some(requests)
                }
                _ => None,
            })
            .expect("the page's picture asked for");
        let request = requests[0].clone();
        let event = {
            let mut held = pictures.lock().unwrap_or_else(PoisonError::into_inner);
            let (cached, pending) = held.cached(&model.browse.model, requests);
            assert!(cached.is_empty() && pending.len() == 1);
            held.arrived(
                &model.browse.model,
                request.link,
                request.target,
                lnomad::gui::Picture::Bytes(png_of(240, 96)),
            )
        };
        loquax::ui::browse::app(&mut model, event);
        let _ = app.update(Message::Seen(seen(&model, 1)));
        let page = shoot(&app, w, h);
        assert_ne!(page, list);
        map::tests::same_as("gui-browse-page.png", &page, w, h);

        // The wheel and the pointer are the model's mouse.
        let _ = app.update(Message::Browse(lnomad::gui::Message::Wheel(3.0)));
        let _ = app.update(Message::Browse(lnomad::gui::Message::Pointer(None)));
        let said = told(&mut rx);
        assert_eq!(
            said,
            vec![
                UiEvent::Mouse(lnomad::gui::wheel_mice(3.0)[0]),
                UiEvent::Mouse(lnomad::gui::pointer_mouse(None)),
            ]
        );

        // Leaving for the chats tells the model; the window stays there
        // through a picture from before, and one from after.
        let before = seen(&model, 1);
        let _ = app.update(Message::Intent(Intent::Tab(TabId::Chats)));
        assert_eq!(told(&mut rx), vec![UiEvent::Tab(TabId::Chats)]);
        assert_eq!(app.screen, Screen::Chats);
        let _ = app.update(Message::Seen(before));
        assert_eq!(app.screen, Screen::Chats, "not pulled back");
        update(&mut model, UiEvent::Tab(TabId::Chats));
        let _ = app.update(Message::Seen(seen(&model, 1)));
        assert_eq!(app.screen, Screen::Chats);
        // A page link in a chat brings it to the front again.
        update(&mut model, UiEvent::Tab(TabId::Browse));
        let _ = app.update(Message::Seen(seen(&model, 1)));
        assert_eq!(app.screen, Screen::Browse);
    }
}
