//! The map's picture in the terminal (`docs/src/concepts/loquax-map.md`,
//! sections 6 and 7, slice (c)): the backend ladder, the region files the
//! reader placed by hand, and the cells the picture and the pins become.
//!
//! The model ([`loquax::ui::map`]) says which view to draw and where the
//! pins go; this module draws the view's picture under them and the
//! attribution line below it.
//!
//! The ladder (1093, Lew 2026-10-28: the most beautiful drawing the
//! terminal can show): kitty or sixel where the terminal answers for one
//! and no mosh or tmux sits between; octants where the terminal draws them
//! itself and the glyphs survive the way there; quadrants and eighth
//! blocks (`blocks`) in a local terminal with 256 colours or more; braille
//! with one-dot lines over cell colours (`braille-thin`) over mosh, in
//! Termux and at 16 colours; plain braille without colour. Half-blocks and
//! the one-colour-a-cell `mosaic` are no rung, only a choice
//! (`docs/drafts/terminal-map-rendering-2026-10.md`). The `--graphics` flag
//! forces a way, `tui.log` names the one taken and why (`GRAPHICS backend=
//! reason=`), beside the `KEYS` line.
//!
//! `t` cycles through every way live, the ones the facts say will not work
//! marked, and the one the reader stays on is remembered per context
//! ([`Facts::context`], the config's `map_drawing.<context>`); a remembered
//! way wins over the ladder at the next start unless it cannot work at all
//! there.
//!
//! The region files, where they are read from, and the frame one view is
//! drawn into are the library's ([`loquax::picture`]), shared with the GUI;
//! the text techniques' encoders are the map core's
//! ([`leviculum_map::cells`]).

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use embedded_graphics::pixelcolor::Rgb565;
use leviculum_map::cells::{self, Encoding, Sets};
use leviculum_map::draw::Ink;
use leviculum_map::labels::{LabelKind, ViewLabel};
use leviculum_map::ladder::{Ladder, Pass};
use leviculum_map::viewport::{Viewport, MAX_VIEW_ZOOM};
use leviculum_mapfill::frame::{rgb888, Frame};
use leviculum_mapfill::style::{MapTheme, Style as MapStyle};
use leviculum_ui::color::Tier;
use leviculum_ui_ratatui::images::{
    self, Backend, Between, FontSize, Picker, Pictures, ProtocolType, SlicedProtocol,
};
use ratatui::buffer::CellDiffOption;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::Frame as TermFrame;
use tokio::sync::Notify;

use loquax::picture::{Drawn, Maps, Painter};
use loquax::ui::map::{place_names, Drawing, MapSetup, MapView};

/// `--graphics`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum GraphicsFlag {
    /// Ask the terminal and climb the ladder, or take the way remembered
    /// for this terminal.
    Auto,
    /// The kitty graphics protocol.
    Kitty,
    /// Sixel.
    Sixel,
    /// Octants: 2 x 4 a cell in two colours, U+1CD00.
    Octant,
    /// Quadrants and eighth blocks, the best fit a cell.
    Blocks,
    /// Two pixels a cell in colour.
    Halfblocks,
    /// Braille with one-dot lines in colour over the areas' colours.
    BrailleThin,
    /// One colour a cell, no glyph.
    Mosaic,
    /// Eight dots a cell, lines only, one colour.
    Braille,
}

/// How the map is drawn.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Graphics {
    Kitty,
    Sixel,
    Octant,
    Blocks,
    Halfblocks,
    BrailleThin,
    Mosaic,
    Braille,
}

impl Graphics {
    /// Every way, best first: the ladder's order and the order `t` cycles
    /// through.
    pub const ORDER: [Graphics; 8] = [
        Self::Kitty,
        Self::Sixel,
        Self::Octant,
        Self::Blocks,
        Self::Halfblocks,
        Self::BrailleThin,
        Self::Mosaic,
        Self::Braille,
    ];

    /// The name `tui.log`, `--graphics`, the map's title and the config use.
    pub fn name(self) -> &'static str {
        match self {
            Self::Kitty => "kitty",
            Self::Sixel => "sixel",
            Self::Octant => "octant",
            Self::Blocks => "blocks",
            Self::Halfblocks => "halfblocks",
            Self::BrailleThin => "braille-thin",
            Self::Mosaic => "mosaic",
            Self::Braille => "braille",
        }
    }

    /// The way `name` names.
    pub fn named(name: &str) -> Option<Self> {
        Self::ORDER.into_iter().find(|g| g.name() == name)
    }

    /// Its place in [`Graphics::ORDER`].
    pub fn index(self) -> usize {
        Self::ORDER.iter().position(|&g| g == self).unwrap_or(0)
    }

    /// Image pixels across and down one cell, as the model lays the view
    /// out. The text techniques of the map core take braille's cell, so a
    /// view keeps its ground when the drawing changes; they draw at twice
    /// that ([`Canvas::source`]).
    pub fn cell_px(self, font: FontSize) -> (u16, u16) {
        match self {
            Self::Kitty | Self::Sixel => (font.width.max(1), font.height.max(1)),
            Self::Halfblocks => (1, 2),
            Self::Octant | Self::Blocks | Self::BrailleThin | Self::Mosaic | Self::Braille => {
                (2, 4)
            }
        }
    }

    fn protocol(self) -> Option<ProtocolType> {
        match self {
            Self::Kitty => Some(ProtocolType::Kitty),
            Self::Sixel => Some(ProtocolType::Sixel),
            _ => None,
        }
    }

    /// The map core's encoder of a text technique.
    fn encoding(self) -> Option<Encoding> {
        match self {
            Self::Octant => Some(Encoding::Octant),
            Self::Blocks => Some(Encoding::BlocksBest),
            Self::BrailleThin => Some(Encoding::BrailleThin),
            Self::Mosaic => Some(Encoding::Mosaic),
            Self::Kitty | Self::Sixel | Self::Halfblocks | Self::Braille => None,
        }
    }

    fn of(flag: GraphicsFlag) -> Option<Self> {
        Some(match flag {
            GraphicsFlag::Auto => return None,
            GraphicsFlag::Kitty => Self::Kitty,
            GraphicsFlag::Sixel => Self::Sixel,
            GraphicsFlag::Octant => Self::Octant,
            GraphicsFlag::Blocks => Self::Blocks,
            GraphicsFlag::Halfblocks => Self::Halfblocks,
            GraphicsFlag::BrailleThin => Self::BrailleThin,
            GraphicsFlag::Mosaic => Self::Mosaic,
            GraphicsFlag::Braille => Self::Braille,
        })
    }
}

/// The first glibc whose `wcwidth` knows the octants (Unicode 16), measured
/// on Debian 13 (1090): a mosh-server on an older one drops them silently.
const OCTANT_GLIBC: (u32, u32) = (2, 41);

/// What the ladder decides on: what the terminal answered, its colour
/// tier, what sits between, and the environment that names the terminal.
#[derive(Clone, Debug, PartialEq)]
pub struct Facts {
    /// The graphics protocol the terminal answered for, when asked.
    pub detected: Option<ProtocolType>,
    pub tier: Tier,
    pub between: Between,
    /// `TERM`.
    pub term: String,
    /// `TERM_PROGRAM`.
    pub program: String,
    /// Termux (`TERMUX_VERSION` set): its default font chain draws braille
    /// and no block elements.
    pub termux: bool,
    /// This host's glibc, asked only behind mosh: the mosh-server's
    /// `wcwidth` decides which glyphs reach the client.
    pub glibc: Option<(u32, u32)>,
}

impl Facts {
    /// The facts with nothing found: no answer, nothing between, no
    /// terminal named. What a key script decides on, so a frame does not
    /// depend on where the test runs.
    pub fn none(tier: Tier) -> Self {
        Self {
            detected: None,
            tier,
            between: Between::default(),
            term: String::new(),
            program: String::new(),
            termux: false,
            glibc: None,
        }
    }

    /// The facts of this process: the terminal's answer and tier as found,
    /// the go-betweens, the environment, and behind mosh the glibc.
    pub fn read(detected: Option<ProtocolType>, tier: Tier, between: Between) -> Self {
        let var = |name: &str| std::env::var(name).unwrap_or_default();
        Self {
            detected,
            tier,
            between,
            term: var("TERM"),
            program: var("TERM_PROGRAM"),
            termux: std::env::var_os("TERMUX_VERSION").is_some(),
            glibc: match between.mosh {
                true => glibc(),
                false => None,
            },
        }
    }

    /// The context a choice is remembered under: what sits between, then
    /// `TERM_PROGRAM`, or `TERM` where that is unset, so a desktop terminal
    /// and a mosh session from the phone each keep their own. Under tmux
    /// `TERM_PROGRAM` is `tmux` itself, so every terminal tmux runs in
    /// shares one.
    pub fn context(&self) -> String {
        let between = match (self.between.mosh, self.between.tmux, self.between.screen) {
            (true, _, _) => "mosh",
            (false, true, _) => "tmux",
            (false, false, true) => "screen",
            _ => "local",
        };
        let term = match (self.program.is_empty(), self.term.is_empty()) {
            (false, _) => self.program.as_str(),
            (true, false) => self.term.as_str(),
            (true, true) => "unknown",
        };
        format!("{between}:{term}")
    }

    /// Whether the terminal draws octants itself, whatever its font: kitty,
    /// foot and WezTerm (their sources, 1090).
    fn draws_octants(&self) -> bool {
        self.term.starts_with("xterm-kitty")
            || self.term.starts_with("foot")
            || self.program == "WezTerm"
    }

    /// Whether an octant reaches the terminal: always without mosh; behind
    /// it only from a glibc that knows them.
    fn octants_arrive(&self) -> bool {
        !self.between.mosh || self.glibc.is_some_and(|v| v >= OCTANT_GLIBC)
    }

    /// Whether a graphics protocol's picture is not taken by a go-between:
    /// mosh drops it, tmux passes it only where configured to.
    fn pictures_arrive(&self) -> bool {
        !self.between.mosh && !self.between.tmux
    }

    /// Whether `graphics` is expected to show here. One that is not is
    /// still offered, marked.
    pub fn expected(&self, graphics: Graphics) -> bool {
        let colour = self.tier != Tier::Mono;
        // The block elements: no Termux default font has them, and over
        // mosh the client's font is unknown (Lew's `▀` came out a box).
        let blocks = colour && !self.termux && !self.between.mosh;
        match graphics {
            Graphics::Kitty => self.detected == Some(ProtocolType::Kitty) && self.pictures_arrive(),
            Graphics::Sixel => self.detected == Some(ProtocolType::Sixel) && self.pictures_arrive(),
            Graphics::Octant => colour && self.draws_octants() && self.octants_arrive(),
            Graphics::Blocks | Graphics::Halfblocks => blocks,
            Graphics::BrailleThin | Graphics::Mosaic => colour,
            Graphics::Braille => true,
        }
    }

    /// Whether `graphics` can work here at all: a picture behind mosh never
    /// arrives, whatever the terminal at the other end can do.
    pub fn possible(&self, graphics: Graphics) -> bool {
        !(graphics.protocol().is_some() && self.between.mosh)
    }

    /// The ways the map is offered in, in [`Graphics::ORDER`], each marked
    /// as expected or not.
    pub fn offers(&self) -> Vec<Drawing> {
        Graphics::ORDER
            .iter()
            .map(|&g| Drawing {
                name: g.name().to_string(),
                expected: self.expected(g),
            })
            .collect()
    }
}

/// This host's glibc version, from `getconf GNU_LIBC_VERSION` ("glibc
/// 2.41"); `None` where there is none (musl, another system) or it does not
/// say.
fn glibc() -> Option<(u32, u32)> {
    let out = std::process::Command::new("getconf")
        .arg("GNU_LIBC_VERSION")
        .output()
        .ok()?;
    parse_glibc(&String::from_utf8_lossy(&out.stdout))
}

/// `glibc 2.41` as `(2, 41)`.
fn parse_glibc(text: &str) -> Option<(u32, u32)> {
    let version = text.trim().strip_prefix("glibc ")?;
    let mut parts = version.split('.');
    let major = parts.next()?.trim().parse().ok()?;
    let minor = parts.next()?.trim().parse().ok()?;
    Some((major, minor))
}

/// The way taken, and the facts that kept it off the rungs above.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chosen {
    pub graphics: Graphics,
    /// `tui.log`'s `reason=`: `forced` for a `--graphics` way, `remembered`
    /// for the config's. Under `auto` without one, each fact once in ladder
    /// order: `no-color`; `no-graphics` (the terminal answered for no kitty
    /// or sixel) or `mosh` or `tmux` (it did, behind one); `no-octants`
    /// (the terminal is not known to draw them) or `old-glibc` (it is, and
    /// a mosh-server would drop them); `termux` or `mosh` (no block
    /// elements); `no-truecolor` (256 colours) or `16-colors`. Empty on the
    /// top rung.
    pub reasons: Vec<&'static str>,
}

impl Chosen {
    /// The reasons as `tui.log` writes them, `best` where there is none.
    pub fn reason(&self) -> String {
        match self.reasons.is_empty() {
            true => "best".to_string(),
            false => self.reasons.join(","),
        }
    }
}

/// The ladder: a forced way as it is; under `auto` kitty or sixel when the
/// terminal answered for one and nothing between takes it, then octants
/// where the terminal draws them itself and they arrive, then `blocks` with
/// 256 colours or more in a local terminal, then `braille-thin` in any
/// colour, plain braille without colour. A terminal that answered for
/// iTerm2's protocol climbs on: the decision names kitty and sixel only.
///
/// Behind mosh no protocol is taken: mosh answers the protocol query itself
/// and drops sixel and kitty pictures (1088). Behind tmux neither: it
/// passes them only where its configuration allows. Nor are block elements
/// taken behind mosh: Lew's map from Termux over mosh (2026-10-28) showed an
/// empty box for every `▀`, and the font at the other end is unknown.
pub fn choose_graphics(flag: GraphicsFlag, facts: &Facts) -> Chosen {
    if let Some(graphics) = Graphics::of(flag) {
        return Chosen {
            graphics,
            reasons: vec!["forced"],
        };
    }
    let mut reasons: Vec<&'static str> = Vec::new();
    let take = |reasons: &mut Vec<&'static str>, why: &'static str| {
        if !reasons.contains(&why) {
            reasons.push(why);
        }
    };
    if facts.tier == Tier::Mono {
        return Chosen {
            graphics: Graphics::Braille,
            reasons: vec!["no-color"],
        };
    }
    match facts.detected {
        Some(ProtocolType::Kitty | ProtocolType::Sixel) if facts.pictures_arrive() => {
            let graphics = match facts.detected {
                Some(ProtocolType::Kitty) => Graphics::Kitty,
                _ => Graphics::Sixel,
            };
            return Chosen { graphics, reasons };
        }
        Some(ProtocolType::Kitty | ProtocolType::Sixel) => {
            if facts.between.mosh {
                take(&mut reasons, "mosh");
            }
            if facts.between.tmux {
                take(&mut reasons, "tmux");
            }
        }
        _ => take(&mut reasons, "no-graphics"),
    }
    let tier = match facts.tier {
        Tier::Ansi256 => Some("no-truecolor"),
        Tier::Ansi16 => Some("16-colors"),
        Tier::Truecolor | Tier::Mono => None,
    };
    let sixteen = facts.tier == Tier::Ansi16;
    match (facts.draws_octants(), facts.octants_arrive()) {
        (true, true) if !sixteen => {
            reasons.extend(tier);
            return Chosen {
                graphics: Graphics::Octant,
                reasons,
            };
        }
        (true, false) => take(&mut reasons, "old-glibc"),
        (false, _) => take(&mut reasons, "no-octants"),
        (true, true) => {}
    }
    if facts.termux {
        take(&mut reasons, "termux");
    }
    if facts.between.mosh {
        take(&mut reasons, "mosh");
    }
    reasons.extend(tier);
    let graphics = match facts.expected(Graphics::Blocks) && !sixteen {
        true => Graphics::Blocks,
        false => Graphics::BrailleThin,
    };
    Chosen { graphics, reasons }
}

/// The way the map starts in: a forced way as it is; under `auto` the way
/// remembered for this context when it can work here, else the ladder.
/// The line beside it says, once, why a remembered way was passed over.
pub fn choose_start(
    flag: GraphicsFlag,
    remembered: Option<&str>,
    facts: &Facts,
) -> (Chosen, Option<String>) {
    let ladder = choose_graphics(flag, facts);
    let Some(name) = remembered.filter(|_| flag == GraphicsFlag::Auto) else {
        return (ladder, None);
    };
    let context = facts.context();
    match Graphics::named(name) {
        Some(graphics) if facts.possible(graphics) => (
            Chosen {
                graphics,
                reasons: vec!["remembered"],
            },
            None,
        ),
        Some(_) => {
            let line = format!(
                "map: {name} was remembered for {context}, but no picture arrives through \
                 mosh; drawn as {} instead",
                ladder.graphics.name()
            );
            (ladder, Some(line))
        }
        None => {
            let line = format!(
                "map: {name} was remembered for {context}, a drawing this Loquax does not \
                 know; drawn as {} instead",
                ladder.graphics.name()
            );
            (ladder, Some(line))
        }
    }
}

/// What the model is told of `painter` under `facts`: its cell and zooms,
/// and every way offered with the one drawing now.
pub fn setup_of(painter: &MapPainter, facts: &Facts) -> MapSetup {
    MapSetup {
        drawings: facts.offers(),
        drawing: painter.graphics().index(),
        ..painter.setup()
    }
}

/// The way of drawing the map the reader chose, and what the config holds
/// for this context: what to write when the reader leaves the map or quits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Memory {
    /// [`Facts::context`].
    pub context: String,
    /// What the config held at start or was last written, as a name.
    saved: Option<String>,
    /// The way drawing now.
    now: Graphics,
    /// Whether the reader chose a way this session: the ladder's own
    /// choice is never written.
    chosen: bool,
}

impl Memory {
    /// The memory of `context`, the config holding `remembered` for it, the
    /// map drawing `now`.
    pub fn new(context: String, remembered: Option<&str>, now: Graphics) -> Self {
        Self {
            context,
            saved: remembered.map(str::to_string),
            now,
            chosen: false,
        }
    }

    /// The reader chose `graphics`.
    pub fn chose(&mut self, graphics: Graphics) {
        self.now = graphics;
        self.chosen = true;
    }

    /// The config key and value owed: the reader's choice, where it is not
    /// what the config holds already.
    pub fn owed(&self) -> Option<(String, &'static str)> {
        let name = self.now.name();
        (self.chosen && self.saved.as_deref() != Some(name)).then(|| {
            (
                format!("{}{}", loquax::config::MAP_DRAWING, self.context),
                name,
            )
        })
    }

    /// What [`Memory::owed`] said was written.
    pub fn written(&mut self) {
        self.saved = Some(self.now.name().to_string());
    }
}

/// The glyph test over the map's `area` (`g`, 1093): a line a glyph set,
/// each as it should look, and the `--graphics` values that need it, so the
/// reader sees which ways their font can draw.
pub fn draw_glyph_test(frame: &mut TermFrame, area: Rect) {
    use ratatui::text::Line;
    use ratatui::widgets::{Block, Borders, Clear, Paragraph};
    let rows = cells::glyph_test();
    let mut lines = vec![Line::from("a box, ? or a gap: the font lacks it")];
    lines.extend(rows.iter().map(|(name, glyphs, rungs)| {
        let glyphs: String = glyphs.iter().collect();
        Line::from(match rungs.is_empty() {
            true => format!("{name:<11} {glyphs}"),
            false => format!("{name:<11} {glyphs}  {rungs}"),
        })
    }));
    let width = lines
        .iter()
        .map(|l| l.width() as u16 + 2)
        .max()
        .unwrap_or(0)
        .min(area.width);
    let height = (lines.len() as u16 + 2).min(area.height);
    let at = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
    .intersection(frame.area());
    frame.render_widget(Clear, at);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" glyph test · g closes ");
    frame.render_widget(Paragraph::new(lines).block(block), at);
}

/// A no-op the terminal carries out before a picture sent again only to
/// erase stale labels: save and restore the cursor, which makes the
/// picture's cell differ from the last frame's so ratatui sends it.
const NUDGE: &str = "\u{1b}7\u{1b}8";

/// One label drawn over the picture.
#[derive(Clone, Debug, PartialEq)]
struct Label {
    col: u16,
    row: u16,
    text: String,
    style: Style,
    /// A name of the map, not a pin's.
    name: bool,
}

/// How a name of the map is set in cells: its own cells' background is its
/// halo, dark text on white in the light table and light on near black in
/// the dark, waters and rivers in blue, parks in green, cities and towns
/// in bold. At sixteen colours the ANSI ones; without colour, plain and
/// bold alone.
fn name_style(tier: Tier, theme: MapTheme) -> impl Fn(LabelKind) -> Style {
    move |kind| {
        let dark = theme == MapTheme::Dark;
        let fg = match (tier, kind, dark) {
            (Tier::Mono, ..) => None,
            (Tier::Ansi16, LabelKind::Water | LabelKind::River | LabelKind::Sea, false) => {
                Some(Color::Indexed(4))
            }
            (Tier::Ansi16, LabelKind::Water | LabelKind::River | LabelKind::Sea, true) => {
                Some(Color::Indexed(12))
            }
            (Tier::Ansi16, LabelKind::Park, false) => Some(Color::Indexed(2)),
            (Tier::Ansi16, LabelKind::Park, true) => Some(Color::Indexed(10)),
            (Tier::Ansi16, _, false) => Some(Color::Indexed(0)),
            (Tier::Ansi16, _, true) => Some(Color::Indexed(15)),
            (_, LabelKind::Water | LabelKind::River | LabelKind::Sea, false) => {
                Some(Color::Rgb(0x1f, 0x4e, 0x8c))
            }
            (_, LabelKind::Water | LabelKind::River | LabelKind::Sea, true) => {
                Some(Color::Rgb(0x8c, 0xb4, 0xe6))
            }
            (_, LabelKind::Park, false) => Some(Color::Rgb(0x2e, 0x6b, 0x2e)),
            (_, LabelKind::Park, true) => Some(Color::Rgb(0x94, 0xc8, 0x8c)),
            (_, _, false) => Some(Color::Rgb(0x20, 0x20, 0x20)),
            (_, _, true) => Some(Color::Rgb(0xe8, 0xe8, 0xe8)),
        };
        let bg = match (tier, dark) {
            (Tier::Mono, _) => None,
            (Tier::Ansi16, false) => Some(Color::Indexed(15)),
            (Tier::Ansi16, true) => Some(Color::Indexed(0)),
            (_, false) => Some(Color::Rgb(0xff, 0xff, 0xff)),
            (_, true) => Some(Color::Rgb(0x16, 0x16, 0x16)),
        };
        let mut style = Style::default();
        if let Some(fg) = fg {
            style = style.fg(fg);
        }
        if let Some(bg) = bg {
            style = style.bg(bg);
        }
        if matches!(kind, LabelKind::City | LabelKind::Town) {
            style = style.add_modifier(Modifier::BOLD);
        }
        style
    }
}

/// How long a picture behind the view is kept on screen while every
/// finished paint is already overtaken by a newer key (1075). A paint that
/// finishes for a view the reader has moved on from is dropped, the newest
/// view wins; but where keys come faster than paints, dropping every one
/// would freeze the map until the key is let go, so after this long the
/// overtaken paint is shown as the nearest there is.
const CATCH_UP_MS: u64 = 100;

/// An encoded picture.
enum Picture {
    /// Kitty, or a sixel through ratatui-image's quantiser: under tmux, or
    /// past 256 colours.
    Sliced(SlicedProtocol),
    /// A sixel of the frame's own colours ([`images::sixel_indexed_direct`]).
    Whole(images::Protocol),
}

/// One view drawn: its pixels, what to credit, and under kitty or sixel
/// the encoded picture for `cells`.
struct Painted {
    view: Viewport,
    cells: (u16, u16),
    /// The style it was drawn in.
    style: MapStyle,
    /// The maps directory's epoch it was painted at.
    epoch: u64,
    /// The time zone view's minute it was painted at (1684).
    time_zones: Option<i64>,
    fb: Vec<u16>,
    attribution: String,
    picture: Option<Picture>,
    /// A text technique's cells, row by row ([`Graphics::encoding`]).
    glyphs: Vec<cells::Cell>,
    /// The names of the view, placed in cells at render time.
    names: Vec<ViewLabel>,
    /// Microseconds the paint took: the frame budget's ground (1422).
    paint_us: u32,
}

/// One paint asked for: the view, the cells its picture covers, and the
/// maps directory's epoch, so a region that landed paints the same view
/// again (1089).
#[derive(Copy, Clone, Debug, PartialEq)]
struct Ask {
    view: Viewport,
    cells: (u16, u16),
    epoch: u64,
    /// The map's style, which follows the theme ([`MapPainter::set_theme`]).
    style: MapStyle,
    /// The time zone view at this minute, in seconds since the epoch
    /// ([`MapPainter::set_time_zones`], 1684).
    time_zones: Option<i64>,
}

/// What draws a view: the regions, the backend, the scratch. It does not
/// know the terminal's frame, so it runs on the painter's own thread as
/// well as inline.
struct Canvas {
    painter: Painter,
    graphics: Graphics,
    pictures: Pictures,
    /// tmux sits between ([`images::in_tmux`]): its passthrough wrapping
    /// is `ratatui-image`'s, so a sixel goes that way. Read once at the
    /// start and handed in, never from the environment here: a test run
    /// from a tmux pane drew a different picture than the same test
    /// anywhere else (1347).
    tmux: bool,
    /// The glyph sets the text techniques fit, built once.
    sets: Sets,
}

impl Canvas {
    /// The ink of this backend: braille draws lines alone.
    fn ink(&self) -> Ink {
        match self.graphics {
            Graphics::Braille => Ink::Lines(Rgb565::new(0, 0, 0)),
            _ => Ink::Colour,
        }
    }

    /// Draw `ask`'s view, and encode it under kitty or sixel or as a text
    /// technique's cells.
    fn paint(&mut self, ask: Ask) -> Painted {
        // The terminal draws whole views, never a cache's square tiles:
        // its world views blend into Equal Earth or the globe (1710).
        self.painter
            .set_world_shape(Some(loquax::picture::world_shape()));
        let started = Instant::now();
        let mut painted = self.paint_view(ask);
        painted.paint_us = u32::try_from(started.elapsed().as_micros()).unwrap_or(u32::MAX);
        painted
    }

    /// [`paint`](Self::paint), untimed.
    fn paint_view(&mut self, ask: Ask) -> Painted {
        let view = &ask.view;
        self.painter.set_time_zones(ask.time_zones);
        // Places, waters and parks, and the streets and rivers, which the
        // cells name where they run level.
        let names = self.painter.labels(view, true);
        if let Some(encoding) = self.graphics.encoding() {
            let (source, attribution) = self.source(view, ask.cells, encoding, ask.style);
            return Painted {
                view: *view,
                cells: ask.cells,
                style: ask.style,
                epoch: ask.epoch,
                time_zones: ask.time_zones,
                fb: Vec::new(),
                attribution,
                picture: None,
                glyphs: cells::encode(&source, encoding, &self.sets),
                names,
                paint_us: 0,
            };
        }
        let (w, h) = view.size();
        let Drawn {
            mut fb,
            attribution,
            ..
        } = self.painter.draw(view, self.ink(), ask.style, 100);
        let picture = match self.graphics {
            Graphics::Kitty | Graphics::Sixel => self.encode(&mut fb, w, h, ask.cells),
            _ => None,
        };
        Painted {
            view: *view,
            cells: ask.cells,
            style: ask.style,
            epoch: ask.epoch,
            time_zones: ask.time_zones,
            fb,
            attribution,
            picture,
            glyphs: Vec::new(),
            names,
            paint_us: 0,
        }
    }

    /// `view` drawn for a text technique over `cells`: at the map core's
    /// source grain of 4 x 8 pixels a cell, twice the model's braille cell,
    /// so one zoom deeper at twice the line width, which keeps the view's
    /// ground and its lines' look (the draft's samples, 1090). At the
    /// deepest zoom, where there is none deeper, the view is drawn at its
    /// own and every pixel doubled. Braille-thin needs the lines alone as
    /// well, at the colour drawing's width and at half of it.
    fn source(
        &mut self,
        view: &Viewport,
        (cols, rows): (u16, u16),
        encoding: Encoding,
        style: MapStyle,
    ) -> (cells::Source, String) {
        let (cols, rows) = (usize::from(cols), usize::from(rows));
        let (fw, fh) = ((cols * cells::SX) as u32, (rows * cells::SY) as u32);
        let deeper = view.zoom() < MAX_VIEW_ZOOM;
        let (fine, scale) = match deeper {
            true => (Viewport::new(view.centre, view.zoom() + 1, fw, fh), 1usize),
            false => (Viewport::new(view.centre, view.zoom(), fw / 2, fh / 2), 2),
        };
        // The source draws at `fine`'s grain but must keep the asked
        // view's blend, `t` included (1710): a world view's cells are
        // the same picture as its pixels, only finer.
        let blend = self.painter.world_blend(view).map(|b| {
            let (w, h) = fine.size();
            b.resized(f64::from(fine.zoom()), w, h)
        });
        let percent = |p: u32| p / scale as u32;
        let mut draw = |ink: Ink, p: u32| -> (Vec<cells::Rgb>, String) {
            let (w, h) = fine.size();
            let mut drawn = self
                .painter
                .draw_with(&fine, blend.as_ref(), ink, style, percent(p));
            let mut flat = Vec::with_capacity(drawn.fb.len() * 3);
            if let Some(f) = Frame::new(&mut drawn.fb, w, h) {
                f.rgb888_into(&mut flat);
            }
            let rgb: Vec<cells::Rgb> = flat.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
            (doubled(rgb, w as usize, scale), drawn.attribution)
        };
        let (rgb, attribution) = draw(Ink::Colour, 200);
        let lines_of = |rgb: Vec<cells::Rgb>| -> Vec<bool> {
            let ground = cells::rgb_of(style.theme.palette().background);
            rgb.into_iter().map(|c| c != ground).collect()
        };
        let (line, thin) = match encoding {
            Encoding::BrailleThin
            | Encoding::BrailleBg
            | Encoding::BrailleColour
            | Encoding::BrailleMono => {
                let ink = Ink::Lines(Rgb565::new(0, 0, 0));
                (lines_of(draw(ink, 200).0), lines_of(draw(ink, 100).0))
            }
            _ => (Vec::new(), Vec::new()),
        };
        let source = cells::Source {
            cols,
            rows,
            rgb,
            line,
            thin,
            theme: style.theme,
        };
        (source, attribution)
    }

    /// The picture of `fb` for `cells`. A sixel of at most 256 colours,
    /// which the map's style always is, is written with those colours as
    /// its palette and no quantiser; anything else goes through
    /// ratatui-image.
    fn encode(&self, fb: &mut [u16], w: u32, h: u32, cells: (u16, u16)) -> Option<Picture> {
        if self.graphics == Graphics::Sixel && !self.tmux {
            if let Some((palette, indices)) = indexed(fb) {
                if let Some(sixel) = images::sixel_indexed_direct(&palette, &indices, w, h, cells) {
                    return Some(Picture::Whole(sixel));
                }
            }
        }
        let mut rgb = Vec::with_capacity(fb.len() * 3);
        if let Some(f) = Frame::new(fb, w, h) {
            f.rgb888_into(&mut rgb);
        }
        images::rgb_image(w, h, rgb)
            .and_then(|image| self.pictures.encode(image, cells).ok())
            .map(Picture::Sliced)
    }
}

/// `rgb`, `w` pixels a row, with every pixel `scale` times across and
/// down.
fn doubled(rgb: Vec<cells::Rgb>, w: usize, scale: usize) -> Vec<cells::Rgb> {
    if scale <= 1 || w == 0 {
        return rgb;
    }
    let mut out = Vec::with_capacity(rgb.len() * scale * scale);
    for row in rgb.chunks(w) {
        let wide: Vec<cells::Rgb> = row
            .iter()
            .flat_map(|&c| std::iter::repeat_n(c, scale))
            .collect();
        for _ in 0..scale {
            out.extend_from_slice(&wide);
        }
    }
    out
}

/// `fb` as a palette of its colours in RGB888 and one index a pixel;
/// `None` past 256 colours.
fn indexed(fb: &[u16]) -> Option<(Vec<[u8; 3]>, Vec<u8>)> {
    const NONE: u16 = u16::MAX;
    let mut slot = vec![NONE; 1 << 16];
    let mut palette = Vec::new();
    let mut indices = Vec::with_capacity(fb.len());
    for &v in fb {
        let mut i = slot[usize::from(v)];
        if i == NONE {
            i = u16::try_from(palette.len()).ok().filter(|&i| i < 256)?;
            slot[usize::from(v)] = i;
            palette.push(rgb888(Rgb565::from(raw(v))));
        }
        indices.push(i as u8);
    }
    Some((palette, indices))
}

/// Between the painter and its thread: the newest paint asked for, which
/// replaces one not yet started, and the last one finished.
#[derive(Default)]
struct Slot {
    asked: Option<Ask>,
    done: Option<Painted>,
    quit: bool,
}

impl Slot {
    /// Ask for `ask`; a paint asked before and not yet started is never
    /// drawn.
    fn ask(&mut self, ask: Ask) {
        self.asked = Some(ask);
    }

    /// The paint to start next, the newest asked.
    fn start(&mut self) -> Option<Ask> {
        self.asked.take()
    }

    /// A paint finished; one finished before and not yet taken is
    /// overtaken by it.
    fn finish(&mut self, painted: Painted) {
        self.done = Some(painted);
    }
}

/// Whether a finished paint is shown: when it is the view wanted now, or
/// when the picture on screen has been there [`CATCH_UP_MS`] or longer
/// (or there is none) and the paint is at least nearer than nothing.
fn keep(done: &Ask, wanted: &Ask, shown_for: Option<Duration>) -> bool {
    done == wanted || shown_for.is_none_or(|d| d >= Duration::from_millis(CATCH_UP_MS))
}

/// The thread the painter draws on, so a key is read while the last one's
/// view is drawn and encoded.
struct Worker {
    shared: Arc<(Mutex<Slot>, Condvar)>,
    ready: Arc<Notify>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// The last paint asked for, so a frame of the same view asks nothing.
    asked: Option<Ask>,
}

impl Worker {
    fn spawn(mut canvas: Canvas) -> Self {
        let shared = Arc::new((Mutex::new(Slot::default()), Condvar::new()));
        let ready = Arc::new(Notify::new());
        let (theirs, notify) = (Arc::clone(&shared), Arc::clone(&ready));
        let thread = std::thread::Builder::new()
            .name("map-painter".to_string())
            .spawn(move || {
                let (slot, wake) = &*theirs;
                loop {
                    let ask = {
                        let Ok(mut held) = slot.lock() else { return };
                        loop {
                            if held.quit {
                                return;
                            }
                            if let Some(ask) = held.start() {
                                break ask;
                            }
                            held = match wake.wait(held) {
                                Ok(held) => held,
                                Err(_) => return,
                            };
                        }
                    };
                    let painted = canvas.paint(ask);
                    let Ok(mut held) = slot.lock() else { return };
                    held.finish(painted);
                    drop(held);
                    notify.notify_one();
                }
            })
            .ok();
        Self {
            shared,
            ready,
            thread,
            asked: None,
        }
    }

    fn ask(&mut self, ask: Ask) {
        if self.asked == Some(ask) {
            return;
        }
        self.asked = Some(ask);
        let (slot, wake) = &*self.shared;
        if let Ok(mut held) = slot.lock() {
            held.ask(ask);
        }
        wake.notify_one();
    }

    fn take(&self) -> Option<Painted> {
        let (slot, _) = &*self.shared;
        slot.lock().ok().and_then(|mut held| held.done.take())
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let (slot, wake) = &*self.shared;
        if let Ok(mut held) = slot.lock() {
            held.quit = true;
        }
        wake.notify_one();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Where views are painted: in the frame that wants them, for a key script
/// and a test whose frames must be the same on every run, or on the
/// worker thread for the terminal.
enum Engine {
    Inline(Box<Canvas>),
    Worker(Worker),
}

/// The backend, the paints, and the last view drawn: a frame that shows
/// the same view again costs nothing, which matters for a sixel encoded
/// whole.
pub struct MapPainter {
    graphics: Graphics,
    maps: Maps,
    setup: MapSetup,
    engine: Engine,
    painted: Option<Painted>,
    /// When the picture on screen was put there, and how many were.
    shown_at: Option<Instant>,
    paints: u64,
    /// The labels of the last frame with the view they were placed for,
    /// and whether its picture carried [`NUDGE`]: flipped when the labels
    /// change, so the picture is sent again then and only then.
    labels: Vec<Label>,
    labels_view: Option<Viewport>,
    nudge: bool,
    /// The screen's colour tier: at 16 colours a text technique's class
    /// colours are drawn in their own of the sixteen ([`cells::ansi16`]).
    tier: Tier,
    /// The map's colour table.
    theme: MapTheme,
    /// The time zone view (1684, [`MapPainter::set_time_zones`]).
    time_zones: bool,
    /// The frame budget (1422): a drag is the gesture, the paint the
    /// ground, the names placed in cells a pass the minimal rung leaves
    /// to the frame at rest.
    ladder: Ladder,
}

impl MapPainter {
    /// A painter over the regions of `maps`, drawing with `graphics` in the
    /// frame that asks. `picker` is the terminal's answer, needed for kitty
    /// and sixel; without one those draw for `ratatui-image`'s assumed 10 x
    /// 20 font.
    pub fn new(maps: Maps, graphics: Graphics, picker: Option<Picker>) -> Self {
        let mut picker = match (graphics.protocol(), picker) {
            (Some(_), Some(picker)) => Some(picker),
            (Some(_), None) => Some(Picker::halfblocks()),
            (None, _) => None,
        };
        let backend = match (graphics.protocol(), picker.as_mut()) {
            (Some(protocol), Some(picker)) => {
                picker.set_protocol_type(protocol);
                Backend::Graphics(protocol)
            }
            _ => Backend::Text,
        };
        let font = picker.as_ref().map_or(
            FontSize {
                width: 8,
                height: 16,
            },
            Picker::font_size,
        );
        let painter = Painter::new(maps.clone());
        let setup = MapSetup {
            cell_px: graphics.cell_px(font),
            // Deeper than the outline where no region is: the radar's
            // zooms (1386).
            zooms: loquax::ui::radar::zooms(painter.zooms()),
            regions: painter.headers(),
            ..MapSetup::default()
        };
        Self {
            graphics,
            maps,
            setup,
            engine: Engine::Inline(Box::new(Canvas {
                painter,
                graphics,
                pictures: Pictures::new(picker, backend),
                tmux: false,
                sets: Sets::default(),
            })),
            painted: None,
            shown_at: None,
            paints: 0,
            labels: Vec::new(),
            labels_view: None,
            nudge: false,
            tier: Tier::Truecolor,
            theme: MapTheme::Light,
            time_zones: false,
            ladder: Ladder::new(),
        }
    }

    /// Hold drags at `rung` whatever they cost: a test's.
    /// Place no names: a measuring tool's ground ([`Painter`]'s own).
    #[cfg(test)]
    fn set_names_off(&mut self, off: bool) {
        if let Engine::Inline(canvas) = &mut self.engine {
            canvas.painter.set_names_off(off);
        }
    }

    #[cfg(test)]
    fn hold_rung(&mut self, rung: Option<u8>) {
        self.ladder.hold(rung);
    }

    /// Draw for a screen in `tier` from the next frame on.
    pub fn set_tier(&mut self, tier: Tier) {
        self.tier = tier;
    }

    /// Draw the map in `theme`'s table from the next frame on; a change
    /// paints the view again.
    pub fn set_theme(&mut self, theme: MapTheme) {
        self.theme = theme;
    }

    /// Draw the time zone view, or not, from the next frame on (1684):
    /// the zones' borders over the picture and each one's clock in place
    /// of the names, painted again as the minute turns.
    pub fn set_time_zones(&mut self, on: bool) {
        self.time_zones = on;
    }

    /// The style a paint asks for: the PC's finish where the picture has
    /// the pixels for casings and round joins (kitty, sixel, octant), the
    /// M9's plain strokes in the coarser techniques.
    fn style(&self) -> MapStyle {
        let finish = match self.graphics {
            Graphics::Kitty | Graphics::Sixel | Graphics::Octant => MapStyle::PC,
            _ => MapStyle::M9,
        };
        finish.in_theme(self.theme)
    }

    /// The same painter, drawing for a terminal behind tmux when `tmux`
    /// ([`images::in_tmux`], read by the caller). A painter is made for a
    /// terminal reached directly, so a test's picture is the same wherever
    /// it runs.
    pub fn behind_tmux(mut self, tmux: bool) -> Self {
        if let Engine::Inline(canvas) = &mut self.engine {
            canvas.tmux = tmux;
        }
        self
    }

    /// The same painter drawing on its own thread: a frame shows the
    /// newest paint finished and asks for its own view, and
    /// [`MapPainter::ready`] wakes the loop when that is drawn.
    pub fn threaded(self) -> Self {
        let engine = match self.engine {
            Engine::Inline(canvas) => Engine::Worker(Worker::spawn(*canvas)),
            worker => worker,
        };
        Self { engine, ..self }
    }

    pub fn graphics(&self) -> Graphics {
        self.graphics
    }

    /// What the model is told: the cell's pixels, the zooms the regions
    /// and the outline reach.
    pub fn setup(&self) -> MapSetup {
        self.setup.clone()
    }

    /// How many paints were put on screen.
    pub fn paints(&self) -> u64 {
        self.paints
    }

    /// The view of the picture on screen.
    #[cfg(test)]
    pub fn painted_view(&self) -> Option<Viewport> {
        self.painted.as_ref().map(|p| p.view)
    }

    /// Resolves when the thread finished a paint; never inline.
    pub async fn ready(&self) {
        match &self.engine {
            Engine::Worker(worker) => worker.ready.notified().await,
            Engine::Inline(_) => std::future::pending().await,
        }
    }

    /// Make `ask` the picture: drawn now inline; on the thread asked for,
    /// and the newest paint finished taken when [`keep`] says so.
    fn paint(&mut self, ask: Ask, now: Instant) {
        let current = |p: &Option<Painted>| {
            p.as_ref().is_some_and(|p| {
                p.view == ask.view
                    && p.cells == ask.cells
                    && p.epoch == ask.epoch
                    && p.style == ask.style
                    && p.time_zones == ask.time_zones
            })
        };
        match &mut self.engine {
            Engine::Inline(canvas) => {
                if !current(&self.painted) {
                    self.painted = Some(canvas.paint(ask));
                    self.shown_at = Some(now);
                    self.paints += 1;
                }
            }
            Engine::Worker(worker) => {
                if let Some(done) = worker.take() {
                    let shown_for = self.shown_at.map(|t| now.saturating_duration_since(t));
                    let of = Ask {
                        view: done.view,
                        cells: done.cells,
                        epoch: done.epoch,
                        style: done.style,
                        time_zones: done.time_zones,
                    };
                    if keep(&of, &ask, shown_for) {
                        self.painted = Some(done);
                        self.shown_at = Some(now);
                        self.paints += 1;
                    }
                }
                if !current(&self.painted) {
                    worker.ask(ask);
                }
            }
        }
    }

    /// Draw the open map: the picture over `shown.area`, the pins over it,
    /// the attribution on the row below. On the thread the picture may be
    /// a paint behind `shown`'s view for a frame; its labels are then the
    /// ones placed for it, so the pins never stand off the streets.
    pub fn render(&mut self, frame: &mut TermFrame, shown: &MapView, muted: Style) {
        let ask = Ask {
            view: shown.viewport,
            cells: (shown.area.width, shown.area.height),
            epoch: self.maps.epoch(),
            style: self.style(),
            time_zones: self.time_zones.then(loquax::zoneclock::unix_minute),
        };
        let started = Instant::now();
        match shown.moving {
            true => self.ladder.gesture_start(),
            false => self.ladder.gesture_end(),
        }
        let paints = self.paints;
        self.paint(ask, started);
        // A new picture's paint is the ground's time; a frame that only
        // places labels over the last one has none.
        let ground_us = (self.paints != paints)
            .then(|| self.painted.as_ref().map(|p| p.paint_us))
            .flatten();
        if let Some(us) = ground_us {
            self.ladder.observe(Pass::Ground, us);
        }
        let place = !shown.moving || self.ladder.motion_layers().has(Pass::Labels);
        self.render_painted(frame, shown, muted, ask, place);
        if let Some(us) = ground_us.filter(|_| shown.moving) {
            let own = u32::try_from(started.elapsed().as_micros()).unwrap_or(u32::MAX);
            self.ladder.motion_frame(us.saturating_add(own));
        }
    }

    /// [`render`](Self::render) once `ask` is asked of the painter: the
    /// map's names placed only with `place`.
    fn render_painted(
        &mut self,
        frame: &mut TermFrame,
        shown: &MapView,
        muted: Style,
        ask: Ask,
        place: bool,
    ) {
        let graphics = self.graphics;
        // The view is the model's, laid out for the size it last heard of;
        // the frame is the terminal's now. Between a shrink and its resize
        // event they differ, and nothing is written outside the frame
        // (1087).
        let bounds = frame.area();
        let area = Rect::new(
            shown.area.x,
            shown.area.y,
            shown.area.width,
            shown.area.height,
        )
        .intersection(bounds);
        let Some(painted) = self.painted.as_ref() else {
            return;
        };
        let (w, _) = painted.view.size();
        let px = |x: u32, y: u32| -> Color {
            let i = (y as usize) * (w as usize) + x as usize;
            let raw = painted.fb.get(i).copied().unwrap_or(0);
            let c = embedded_graphics::pixelcolor::raw::RawU16::new(u16::from_be(raw));
            let [r, g, b] = rgb888(Rgb565::from(c));
            Color::Rgb(r, g, b)
        };
        // A picture painted for another pane size is not drawn over this
        // one: its cells would not cover the pane.
        let fits = painted.cells == ask.cells && (area.width, area.height) == ask.cells;
        // No region here (1386): the radar's blank ground, whatever the
        // technique; its rings are drawn with the tracks below.
        let radar = shown.radar.as_ref();
        match graphics {
            _ if radar.is_some() => {
                let buf = frame.buffer_mut();
                for row in 0..area.height {
                    for col in 0..area.width {
                        if let Some(cell) = buf.cell_mut((area.x + col, area.y + row)) {
                            cell.reset();
                            cell.set_char(' ');
                        }
                    }
                }
            }
            _ if !fits => {}
            Graphics::Halfblocks => {
                let buf = frame.buffer_mut();
                for row in 0..area.height {
                    for col in 0..area.width {
                        let (x, y) = (u32::from(col), u32::from(row) * 2);
                        if let Some(cell) = buf.cell_mut((area.x + col, area.y + row)) {
                            cell.set_symbol("▀").set_fg(px(x, y)).set_bg(px(x, y + 1));
                        }
                    }
                }
            }
            Graphics::Braille => {
                let ground = painted.style.theme.palette().background;
                let ink = |x: u32, y: u32| {
                    let i = (y as usize) * (w as usize) + x as usize;
                    painted
                        .fb
                        .get(i)
                        .is_some_and(|&v| Rgb565::from(raw(v)) != ground)
                };
                let buf = frame.buffer_mut();
                for row in 0..area.height {
                    for col in 0..area.width {
                        let (x, y) = (u32::from(col) * 2, u32::from(row) * 4);
                        let dots = braille(|dx, dy| ink(x + dx, y + dy));
                        if let Some(cell) = buf.cell_mut((area.x + col, area.y + row)) {
                            cell.set_char(dots);
                        }
                    }
                }
            }
            Graphics::Kitty | Graphics::Sixel => match &painted.picture {
                Some(Picture::Sliced(picture)) => images::render(frame, area, picture, 0, 0),
                Some(Picture::Whole(picture)) => images::render_whole(frame, area, picture),
                None => {}
            },
            Graphics::Octant | Graphics::Blocks | Graphics::BrailleThin | Graphics::Mosaic => {
                let (tier, theme) = (self.tier, painted.style.theme);
                let colour = |c: Option<cells::Rgb>| match (c, tier) {
                    (None, _) => Color::Reset,
                    (Some(rgb), Tier::Ansi16) => match cells::ansi16(rgb, theme) {
                        Some(i) => Color::Indexed(i),
                        None => Color::Rgb(rgb[0], rgb[1], rgb[2]),
                    },
                    (Some([r, g, b]), _) => Color::Rgb(r, g, b),
                };
                let cols = usize::from(painted.cells.0);
                let buf = frame.buffer_mut();
                for row in 0..area.height {
                    for col in 0..area.width {
                        let i = usize::from(row) * cols + usize::from(col);
                        let Some(c) = painted.glyphs.get(i) else {
                            continue;
                        };
                        // The blank braille pattern is a glyph a font may
                        // lack; a space shows the same background.
                        let ch = match c.ch {
                            '\u{2800}' => ' ',
                            ch => ch,
                        };
                        if let Some(cell) = buf.cell_mut((area.x + col, area.y + row)) {
                            cell.set_char(ch).set_fg(colour(c.fg)).set_bg(colour(c.bg));
                        }
                    }
                }
            }
        }
        // The labels placed for a paint of another pane size belong to
        // that size's layout, not to this one: a shrink would put them
        // below the frame (1087).
        let behind = fits && painted.view != ask.view && self.labels_view == Some(painted.view);
        let pins = match behind {
            true => self.labels.iter().filter(|l| !l.name).cloned().collect(),
            false => labels_of(shown, muted),
        };
        // The map's names go where no pin and no pin's name is, placed for
        // the picture's own view so they never stand off its streets.
        let taken: Vec<(u16, u16, u16)> = pins
            .iter()
            .filter_map(|l| {
                let col = l.col.checked_sub(area.x)?;
                let row = l.row.checked_sub(area.y)?;
                Some((col, row, l.text.chars().count() as u16))
            })
            .collect();
        let names = match fits && radar.is_none() && place {
            true => {
                let at = leviculum_ui::Rect::new(area.x, area.y, area.width, area.height);
                let started = Instant::now();
                let names = place_names(&painted.names, at, self.setup.cell_px, &taken);
                let us = u32::try_from(started.elapsed().as_micros()).unwrap_or(u32::MAX);
                self.ladder.observe(Pass::Labels, us);
                names
            }
            false => Vec::new(),
        };
        let halo = name_style(self.tier, painted.style.theme);
        let mut labels: Vec<Label> = names
            .into_iter()
            .map(|n| Label {
                col: n.col,
                row: n.row,
                style: halo(n.kind),
                text: n.text,
                name: true,
            })
            .collect();
        // The tracks under the pins and over the map's names (1385).
        let dark = matches!(painted.style.theme, MapTheme::Dark);
        if let Some(r) = radar {
            labels.extend(r.cells.iter().map(|t| Label {
                col: t.col,
                row: t.row,
                style: track_style(self.tier, dark, t.hue, t.level),
                text: t.text.clone(),
                name: false,
            }));
        }
        if fits || radar.is_some() {
            labels.extend(shown.tracks.iter().map(|t| Label {
                col: t.col,
                row: t.row,
                style: track_style(self.tier, dark, t.hue, t.level),
                text: t.text.clone(),
                name: false,
            }));
        }
        labels.extend(pins);
        let buf = frame.buffer_mut();
        if matches!(graphics, Graphics::Kitty | Graphics::Sixel) {
            // A graphics protocol marks the cells under its picture as
            // skipped, so ratatui never rewrites the cells a label left:
            // the terminal keeps the old text there. When the labels moved,
            // the picture is sent again, its erase clearing them (1074).
            if labels != self.labels {
                self.nudge = !self.nudge;
            }
            if self.nudge {
                for row in area.y..area.y + area.height {
                    if let Some(cell) = buf.cell_mut((area.x, row)) {
                        if matches!(cell.diff_option, CellDiffOption::ForcedWidth(_)) {
                            let nudged = format!("{NUDGE}{}", cell.symbol());
                            cell.set_symbol(&nudged);
                        }
                    }
                }
            }
        }
        for label in &labels {
            let (col, row) = (label.col, label.row);
            if !area.contains(ratatui::layout::Position::new(col, row)) {
                continue;
            }
            let room = usize::from((area.x + area.width).saturating_sub(col));
            let (end, _) = buf.set_stringn(col, row, &label.text, room, label.style);
            // Drawn over the picture every frame: a picture sent again
            // erased the label even where its cells did not change, and
            // a blank in it is the same blank as last frame's.
            for x in col..end {
                if let Some(cell) = buf.cell_mut((x, row)) {
                    cell.set_diff_option(CellDiffOption::AlwaysUpdate);
                }
            }
        }
        if fits && !behind {
            self.labels_view = Some(painted.view);
        }
        self.labels = labels;
        // The region's own line, "© OpenStreetMap contributors", or the
        // outline's; always on the view. A time window that is not the
        // default goes before it (1419): the reason older lines show.
        let line = match radar {
            Some(r) => r.note.as_str(),
            None => painted.attribution.as_str(),
        };
        let credit = match &shown.window_word {
            Some(word) => format!("{word} · {line} "),
            None => format!("{line} "),
        };
        let len = credit.chars().count() as u16;
        let x = (area.x + area.width).saturating_sub(len).max(area.x);
        if area.y + area.height >= bounds.bottom() {
            return;
        }
        buf.set_stringn(
            x,
            area.y + area.height,
            &credit,
            usize::from(area.width),
            muted,
        );
    }
}

/// A track cell's style: the hue, brighter or fainter by its level of
/// four ([`loquax::ui::tracks::Age::level`]): full, the hue blended a
/// third, two thirds and three quarters into the map's ground; at sixteen
/// colours the hue's ANSI colour, dim below the top two levels; no colour
/// at all on a mono terminal, dim below the top two.
fn track_style(tier: Tier, dark: bool, hue: Option<u8>, level: u8) -> Style {
    use loquax::ui::tracks;
    let style = Style::default();
    match tier {
        Tier::Mono => match level {
            2..=3 => style,
            _ => style.add_modifier(Modifier::DIM),
        },
        Tier::Ansi16 => {
            let style = style.fg(Color::Indexed(tracks::ansi16(hue, dark)));
            match level {
                2..=3 => style,
                _ => style.add_modifier(Modifier::DIM),
            }
        }
        _ => {
            let [r, g, b] = tracks::colour(hue, dark);
            let ground: [u8; 3] = match dark {
                true => [0x16, 0x16, 0x16],
                false => [0xf2, 0xef, 0xe9],
            };
            let keep = [1.0, 0.65, 0.4, 0.25][usize::from(3 - level.min(3))];
            let mix =
                |c: u8, g: u8| (f32::from(c) * keep + f32::from(g) * (1.0 - keep)).round() as u8;
            style.fg(Color::Rgb(
                mix(r, ground[0]),
                mix(g, ground[1]),
                mix(b, ground[2]),
            ))
        }
    }
}

/// The labels of `shown`'s pins, styled.
fn labels_of(shown: &MapView, muted: Style) -> Vec<Label> {
    let mut labels = Vec::new();
    for pin in &shown.pins {
        let mut style = Style::default();
        // A node nearby that is no contact (1302): a neutral pin, in the
        // muted grey, its announced name beside it.
        if pin.nearby {
            style = style.patch(muted);
        }
        if pin.stale {
            style = style.patch(muted).add_modifier(Modifier::DIM);
        }
        if pin.selected {
            style = style.add_modifier(Modifier::REVERSED);
        }
        let apart = pin.apart.iter().map(|a| (a.col, a.row, a.text.clone()));
        labels.extend(
            std::iter::once((pin.col, pin.row, pin.text.clone()))
                .chain(apart)
                .map(|(col, row, text)| Label {
                    col,
                    row,
                    text,
                    style,
                    name: false,
                }),
        );
    }
    labels
}

fn raw(v: u16) -> embedded_graphics::pixelcolor::raw::RawU16 {
    embedded_graphics::pixelcolor::raw::RawU16::new(u16::from_be(v))
}

/// The braille cell whose dots `ink(dx, dy)` sets, dx in 0..2, dy in 0..4;
/// a blank where none is.
fn braille(ink: impl Fn(u32, u32) -> bool) -> char {
    // The Unicode braille bit of each dot, column by column.
    const BITS: [[u32; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];
    let mut bits = 0u32;
    for (dx, column) in BITS.iter().enumerate() {
        for (dy, bit) in column.iter().enumerate() {
            if ink(dx as u32, dy as u32) {
                bits |= bit;
            }
        }
    }
    match bits {
        0 => ' ',
        bits => char::from_u32(0x2800 + bits).unwrap_or(' '),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_map::mercator::{project, LatLon, World};
    use loquax::ui::map::PinCell;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::path::PathBuf;

    /// The facts of a terminal named by `TERM` and `TERM_PROGRAM`.
    fn facts(
        detected: Option<ProtocolType>,
        tier: Tier,
        between: Between,
        (term, program): (&str, &str),
        termux: bool,
        glibc: Option<(u32, u32)>,
    ) -> Facts {
        Facts {
            detected,
            tier,
            between,
            term: term.to_string(),
            program: program.to_string(),
            termux,
            glibc,
        }
    }

    /// Every combination of what the terminal answered, its colour tier,
    /// what sits between, the terminal's name, Termux and the glibc behind
    /// mosh, under `auto`, against the way and the reasons the ladder of
    /// 1093 names, written out here as the draft states it.
    #[test]
    fn the_ladder_takes_pictures_then_octants_then_blocks_then_braille_thin() {
        use GraphicsFlag::Auto;
        use ProtocolType::{Halfblocks as None_, Iterm2, Kitty, Sixel};
        let direct = Between::default();
        let mosh = Between {
            mosh: true,
            ..direct
        };
        let tmux = Between {
            tmux: true,
            ..direct
        };
        let screen = Between {
            screen: true,
            ..direct
        };
        let detections = [None, Some(None_), Some(Iterm2), Some(Kitty), Some(Sixel)];
        let tiers = [Tier::Mono, Tier::Ansi16, Tier::Ansi256, Tier::Truecolor];
        let terms = [
            ("xterm-256color", ""),
            ("foot", ""),
            ("xterm-kitty", ""),
            ("xterm-256color", "WezTerm"),
            ("", ""),
        ];
        let mut rows = 0;
        for between in [direct, mosh, tmux, screen] {
            for detected in detections {
                for tier in tiers {
                    for term in terms {
                        for termux in [false, true] {
                            for glibc in [None, Some((2, 36)), Some((2, 41))] {
                                let f = facts(detected, tier, between, term, termux, glibc);
                                let chosen = choose_graphics(Auto, &f);
                                let want = ladder_by_hand(&f);
                                assert_eq!(
                                    (chosen.graphics, chosen.reasons.clone()),
                                    want,
                                    "{f:?}"
                                );
                                rows += 1;
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(rows, 4 * 5 * 4 * 5 * 2 * 3);
        let named = |f: Facts| {
            let c = choose_graphics(Auto, &f);
            (c.graphics, c.reason())
        };
        // Lew's line from Termux over mosh, 2026-10-28, and what it is now.
        assert_eq!(
            named(facts(
                Some(None_),
                Tier::Ansi256,
                mosh,
                ("xterm-256color", ""),
                false,
                Some((2, 41))
            )),
            (
                Graphics::BrailleThin,
                "no-graphics,no-octants,mosh,no-truecolor".into()
            )
        );
        // foot on foot: its sixel; behind tmux octants are unknown there.
        let foot = ("foot", "");
        assert_eq!(
            named(facts(
                Some(Sixel),
                Tier::Truecolor,
                direct,
                foot,
                false,
                None
            )),
            (Graphics::Sixel, "best".into())
        );
        assert_eq!(
            named(facts(
                Some(Sixel),
                Tier::Truecolor,
                tmux,
                ("tmux-256color", "tmux"),
                false,
                None
            )),
            (Graphics::Blocks, "tmux,no-octants".into())
        );
        // foot over mosh: octants from a server with glibc 2.41, not 2.36.
        assert_eq!(
            named(facts(
                Some(None_),
                Tier::Truecolor,
                mosh,
                foot,
                false,
                Some((2, 41))
            )),
            (Graphics::Octant, "no-graphics".into())
        );
        assert_eq!(
            named(facts(
                Some(None_),
                Tier::Truecolor,
                mosh,
                foot,
                false,
                Some((2, 36))
            )),
            (Graphics::BrailleThin, "no-graphics,old-glibc,mosh".into())
        );
        // WezTerm without a picture protocol answered; a plain xterm.
        assert_eq!(
            named(facts(
                None,
                Tier::Truecolor,
                direct,
                ("xterm-256color", "WezTerm"),
                false,
                None
            )),
            (Graphics::Octant, "no-graphics".into())
        );
        assert_eq!(
            named(facts(
                Some(None_),
                Tier::Ansi256,
                direct,
                ("xterm-256color", ""),
                false,
                None
            )),
            (
                Graphics::Blocks,
                "no-graphics,no-octants,no-truecolor".into()
            )
        );
        // Termux itself, and 16 colours anywhere.
        assert_eq!(
            named(facts(
                None,
                Tier::Truecolor,
                direct,
                ("xterm-256color", ""),
                true,
                None
            )),
            (
                Graphics::BrailleThin,
                "no-graphics,no-octants,termux".into()
            )
        );
        assert_eq!(
            named(facts(None, Tier::Ansi16, direct, foot, false, None)),
            (Graphics::BrailleThin, "no-graphics,16-colors".into())
        );
        // A forced way is taken as it is, whatever was found.
        for graphics in Graphics::ORDER {
            let flag = match graphics {
                Graphics::Kitty => GraphicsFlag::Kitty,
                Graphics::Sixel => GraphicsFlag::Sixel,
                Graphics::Octant => GraphicsFlag::Octant,
                Graphics::Blocks => GraphicsFlag::Blocks,
                Graphics::Halfblocks => GraphicsFlag::Halfblocks,
                Graphics::BrailleThin => GraphicsFlag::BrailleThin,
                Graphics::Mosaic => GraphicsFlag::Mosaic,
                Graphics::Braille => GraphicsFlag::Braille,
            };
            for between in [direct, mosh] {
                for tier in tiers {
                    let f = facts(Some(Kitty), tier, between, foot, false, None);
                    let chosen = choose_graphics(flag, &f);
                    assert_eq!(
                        (chosen.graphics, chosen.reason()),
                        (graphics, "forced".into())
                    );
                }
            }
        }
    }

    /// The ladder as the draft and 1093 state it, rung by rung.
    fn ladder_by_hand(f: &Facts) -> (Graphics, Vec<&'static str>) {
        if f.tier == Tier::Mono {
            return (Graphics::Braille, vec!["no-color"]);
        }
        let (mosh, tmux) = (f.between.mosh, f.between.tmux);
        let mut why: Vec<&'static str> = Vec::new();
        let mut add = |w: &'static str| {
            if !why.contains(&w) {
                why.push(w);
            }
        };
        match f.detected {
            Some(ProtocolType::Kitty) if !mosh && !tmux => return (Graphics::Kitty, Vec::new()),
            Some(ProtocolType::Sixel) if !mosh && !tmux => return (Graphics::Sixel, Vec::new()),
            Some(ProtocolType::Kitty | ProtocolType::Sixel) => {
                if mosh {
                    add("mosh");
                }
                if tmux {
                    add("tmux");
                }
            }
            _ => add("no-graphics"),
        }
        let tier = match f.tier {
            Tier::Ansi256 => Some("no-truecolor"),
            Tier::Ansi16 => Some("16-colors"),
            _ => None,
        };
        let draws = f.term == "foot" || f.term == "xterm-kitty" || f.program == "WezTerm";
        let arrive = !mosh || f.glibc == Some((2, 41));
        if draws && arrive && f.tier != Tier::Ansi16 {
            why.extend(tier);
            return (Graphics::Octant, why);
        }
        if draws && !arrive {
            add("old-glibc");
        }
        if !draws {
            add("no-octants");
        }
        if f.termux {
            add("termux");
        }
        if mosh {
            add("mosh");
        }
        why.extend(tier);
        match !f.termux && !mosh && f.tier != Tier::Ansi16 {
            true => (Graphics::Blocks, why),
            false => (Graphics::BrailleThin, why),
        }
    }

    /// A remembered way wins over the ladder; one that cannot work here (a
    /// picture behind mosh) or that this loquax does not know falls back to
    /// the ladder, with one line saying so; a forced way wins over both.
    #[test]
    fn a_remembered_way_wins_unless_it_cannot_work_here() {
        let foot = ("foot", "");
        let local = facts(None, Tier::Truecolor, Between::default(), foot, false, None);
        let mosh = facts(
            None,
            Tier::Truecolor,
            Between {
                mosh: true,
                ..Between::default()
            },
            ("xterm-256color", ""),
            false,
            None,
        );
        let (chosen, line) = choose_start(GraphicsFlag::Auto, Some("mosaic"), &local);
        assert_eq!(
            (chosen.graphics, chosen.reason(), line),
            (Graphics::Mosaic, "remembered".into(), None)
        );
        let (chosen, line) = choose_start(GraphicsFlag::Auto, Some("braille-thin"), &mosh);
        assert_eq!((chosen.graphics, line), (Graphics::BrailleThin, None));
        let (chosen, line) = choose_start(GraphicsFlag::Auto, Some("sixel"), &mosh);
        assert_eq!(chosen.graphics, Graphics::BrailleThin);
        assert_eq!(chosen.reason(), "no-graphics,no-octants,mosh");
        let line = line.expect("said once");
        assert!(
            line.contains("sixel was remembered for mosh:xterm-256color"),
            "{line}"
        );
        assert!(line.contains("drawn as braille-thin"), "{line}");
        let (chosen, line) = choose_start(GraphicsFlag::Auto, Some("hologram"), &local);
        assert_eq!(chosen.graphics, Graphics::Octant);
        assert!(line.is_some_and(|l| l.contains("does not know")));
        let (chosen, line) = choose_start(GraphicsFlag::Braille, Some("octant"), &local);
        assert_eq!(
            (chosen.graphics, chosen.reason(), line),
            (Graphics::Braille, "forced".into(), None)
        );
        let (chosen, line) = choose_start(GraphicsFlag::Auto, None, &local);
        assert_eq!((chosen.graphics, line), (Graphics::Octant, None));
    }

    /// The context a choice is kept under: what sits between, then
    /// `TERM_PROGRAM` or else `TERM`.
    #[test]
    fn the_context_names_the_go_between_and_the_terminal() {
        let at = |between: Between, term: (&str, &str)| {
            facts(None, Tier::Truecolor, between, term, false, None).context()
        };
        let direct = Between::default();
        let mosh = Between {
            mosh: true,
            tmux: true,
            ..direct
        };
        let tmux = Between {
            tmux: true,
            ..direct
        };
        assert_eq!(at(direct, ("foot", "")), "local:foot");
        assert_eq!(at(direct, ("xterm-256color", "WezTerm")), "local:WezTerm");
        assert_eq!(at(mosh, ("xterm-256color", "")), "mosh:xterm-256color");
        assert_eq!(at(tmux, ("tmux-256color", "tmux")), "tmux:tmux");
        assert_eq!(at(direct, ("", "")), "local:unknown");
        assert_eq!(parse_glibc("glibc 2.41\n"), Some((2, 41)));
        assert_eq!(parse_glibc("glibc 2.36"), Some((2, 36)));
        assert_eq!(parse_glibc(""), None);
        assert_eq!(parse_glibc("musl"), None);
    }

    /// What is written: nothing until the reader chooses, then the choice
    /// once under the context's key, nothing while it is what the config
    /// holds.
    #[test]
    fn the_memory_writes_the_readers_choice_once() {
        let mut memory = Memory::new("local:foot".to_string(), None, Graphics::Octant);
        assert_eq!(memory.owed(), None, "the ladder's choice is not written");
        memory.chose(Graphics::Blocks);
        assert_eq!(
            memory.owed(),
            Some(("map_drawing.local:foot".to_string(), "blocks"))
        );
        memory.written();
        assert_eq!(memory.owed(), None);
        memory.chose(Graphics::Blocks);
        assert_eq!(memory.owed(), None, "the same again is not written");
        let mut held = Memory::new("mosh:x".to_string(), Some("mosaic"), Graphics::Mosaic);
        held.chose(Graphics::Mosaic);
        assert_eq!(held.owed(), None, "already in the config");
        held.chose(Graphics::Braille);
        assert_eq!(
            held.owed(),
            Some(("map_drawing.mosh:x".to_string(), "braille"))
        );
        // Every name reads back as its way.
        for g in Graphics::ORDER {
            assert_eq!(Graphics::named(g.name()), Some(g));
            assert_eq!(Graphics::ORDER[g.index()], g);
        }
    }

    /// The ways offered: all of them, in the ladder's order, those the
    /// facts do not expect to show marked.
    #[test]
    fn every_way_is_offered_the_unlikely_ones_marked() {
        let mosh = Between {
            mosh: true,
            ..Between::default()
        };
        let f = facts(
            Some(ProtocolType::Sixel),
            Tier::Truecolor,
            mosh,
            ("foot", ""),
            false,
            Some((2, 36)),
        );
        let offers = f.offers();
        let names: Vec<&str> = offers.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "kitty",
                "sixel",
                "octant",
                "blocks",
                "halfblocks",
                "braille-thin",
                "mosaic",
                "braille"
            ]
        );
        let expected: Vec<bool> = offers.iter().map(|d| d.expected).collect();
        assert_eq!(
            expected,
            [false, false, false, false, false, true, true, true]
        );
        assert!(!f.possible(Graphics::Sixel) && f.possible(Graphics::Octant));
        let local = facts(
            Some(ProtocolType::Sixel),
            Tier::Truecolor,
            Between::default(),
            ("foot", ""),
            false,
            None,
        );
        let expected: Vec<bool> = local.offers().iter().map(|d| d.expected).collect();
        assert_eq!(expected, [false, true, true, true, true, true, true, true]);
    }

    /// Half-blocks at 256 colours reach the terminal as palette indexes
    /// only: the frame's downgrade, which the screen runs over every
    /// frame, turns the map's RGB into `38;5`/`48;5`, and no `38;2` or
    /// `48;2` is written that a 256 colour terminal would misread.
    #[test]
    fn halfblocks_at_256_colours_emit_only_palette_sgr() {
        use leviculum_ui::color::ColorDepth;
        use leviculum_ui_ratatui::style::downgrade_buffer;
        use ratatui::backend::CrosstermBackend;
        let mut painter = MapPainter::new(Maps::default(), Graphics::Halfblocks, None);
        let shown = shown(painter.setup().cell_px);
        // The backend's writer is private in ratatui; a shared buffer
        // keeps what it wrote.
        #[derive(Clone, Default)]
        struct Shared(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);
        impl std::io::Write for Shared {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.borrow_mut().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let bytes = Shared::default();
        // A fixed viewport: the size is not asked of a terminal there is not.
        let options = ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Fixed(Rect::new(0, 0, 32, 10)),
        };
        let mut terminal = Terminal::with_options(CrosstermBackend::new(bytes.clone()), options)
            .expect("terminal");
        terminal
            .draw(|f| {
                painter.render(f, &shown, Style::default());
                downgrade_buffer(f, ColorDepth::Ansi256, false);
            })
            .expect("draw");
        let out = String::from_utf8_lossy(&bytes.0.borrow()).into_owned();
        // The 30 x 8 picture: its blocks, the letters of the world's
        // names over it (1683), less the pin label.
        let letters = out.chars().filter(|c| c.is_alphanumeric()).count();
        assert!(out.matches('▀').count() + letters >= 30 * 8 - 20, "{out:?}");
        let sgr: Vec<&str> = out
            .split("\u{1b}[")
            .skip(1)
            .filter_map(|s| s.split_once('m').map(|(p, _)| p))
            .filter(|p| p.chars().all(|c| c.is_ascii_digit() || c == ';'))
            .collect();
        assert!(sgr.iter().any(|p| p.contains("38;5;")));
        assert!(sgr.iter().any(|p| p.contains("48;5;")));
        for p in &sgr {
            assert!(
                !p.contains("38;2") && !p.contains("48;2"),
                "24-bit SGR {p:?}"
            );
        }
    }

    #[test]
    fn braille_sets_the_dot_of_each_inked_pixel() {
        assert_eq!(braille(|_, _| false), ' ');
        assert_eq!(braille(|_, _| true), '⣿');
        assert_eq!(braille(|dx, dy| dx == 0 && dy == 0), '⠁');
        assert_eq!(braille(|dx, dy| dx == 1 && dy == 3), '⢀');
    }

    /// The outline over Europe at z1, coasts in view, in a 30 x 8 picture,
    /// with one pin.
    fn shown(cell_px: (u16, u16)) -> MapView {
        let area = leviculum_ui::Rect::new(1, 1, 30, 8);
        let at = project(LatLon::from_degrees(48.0, 10.0));
        MapView {
            area,
            viewport: Viewport::new(at, 1, 30 * u32::from(cell_px.0), 8 * u32::from(cell_px.1)),
            cell_px,
            tracks: Vec::new(),
            radar: None,
            window_word: None,
            moving: false,
            pins: vec![PinCell {
                peer: [1; 16],
                col: 16,
                row: 5,
                text: "● bob · 4 min".to_string(),
                apart: None,
                stale: false,
                nearby: false,
                selected: true,
                own: false,
            }],
        }
    }

    fn drawn(graphics: Graphics, picker: Option<Picker>) -> (TestBackend, String) {
        let mut painter = MapPainter::new(Maps::default(), graphics, picker);
        let shown = shown(painter.setup().cell_px);
        let mut terminal = Terminal::new(TestBackend::new(32, 10)).expect("terminal");
        let Ok(_) = terminal.draw(|f| painter.render(f, &shown, Style::default()));
        let backend = terminal.backend().clone();
        let mut all = String::new();
        for y in 0..10 {
            for x in 0..32 {
                if let Some(cell) = backend.buffer().cell((x, y)) {
                    all.push_str(cell.symbol());
                }
            }
            all.push('\n');
        }
        (backend, all)
    }

    /// Kitty and sixel are not cells a snapshot can read; what is checked
    /// is that each produces its protocol's bytes, from a picker that asked
    /// no terminal.
    #[test]
    fn kitty_and_sixel_produce_their_protocol_bytes() {
        let (_, sixel) = drawn(Graphics::Sixel, Some(Picker::halfblocks()));
        assert!(sixel.contains("\u{1b}P"), "a sixel DCS");
        assert!(sixel.contains("● bob"), "the pin over the picture");
        assert!(sixel.contains("Natural Earth"));
        let (_, kitty) = drawn(Graphics::Kitty, None);
        assert!(kitty.contains("\u{1b}_G"), "a kitty APC");
        assert!(kitty.contains("● bob"));
    }

    /// Half-blocks paint every picture cell with the upper half block and
    /// two colours; braille draws the coasts as dots; both carry the pin
    /// and the attribution.
    #[test]
    fn halfblocks_and_braille_are_plain_cells() {
        let (backend, text) = drawn(Graphics::Halfblocks, None);
        let cell = backend.buffer().cell((2, 2)).expect("cell");
        assert_eq!(cell.symbol(), "▀");
        assert!(matches!(cell.fg, Color::Rgb(..)) && matches!(cell.bg, Color::Rgb(..)));
        assert!(text.contains("● bob · 4 min"));
        assert!(text.contains("Natural Earth"));
        let (_, text) = drawn(Graphics::Braille, None);
        assert!(text.chars().any(|c| ('\u{2801}'..='\u{28ff}').contains(&c)));
        assert!(text.contains("● bob · 4 min"));
    }

    /// The time window (1419): no strip, no word with the default; another
    /// window's word on the credit's row, before the credit.
    #[test]
    fn a_window_not_the_default_says_its_word_on_the_credit_row() {
        let (_, plain) = drawn(Graphics::Halfblocks, None);
        assert!(!plain.contains("This week"));
        let mut painter = MapPainter::new(Maps::default(), Graphics::Halfblocks, None);
        let mut view = shown(painter.setup().cell_px);
        view.window_word = Some("This week".to_string());
        let mut terminal = Terminal::new(TestBackend::new(32, 10)).expect("terminal");
        let Ok(_) = terminal.draw(|f| painter.render(f, &view, Style::default()));
        let row: String = (0..32)
            .filter_map(|x| terminal.backend().buffer().cell((x, 9)))
            .map(|c| c.symbol().to_string())
            .collect();
        assert!(row.contains("This week · "), "{row:?}");
    }

    /// The smallest shift by which every column of `cols` repeats, the
    /// copies of the world side by side; `None` where none does.
    fn period<T: PartialEq>(cols: &[T]) -> Option<usize> {
        (1..cols.len()).find(|&p| (0..cols.len() - p).all(|i| cols[i] == cols[i + p]))
    }

    /// The picture of `view` in half-blocks, 1 x 2 pixels a cell, as its
    /// columns of cell colours.
    fn halfblock_columns(view: Viewport) -> Vec<Vec<(Color, Color)>> {
        let mut painter = MapPainter::new(Maps::default(), Graphics::Halfblocks, None);
        // The names are placed per view, not per copy of the world (1683):
        // what repeats is the ground under them.
        painter.set_names_off(true);
        let (w, h) = view.size();
        let (cols, rows) = (w as u16, (h / 2) as u16);
        let shown = MapView {
            tracks: Vec::new(),
            radar: None,
            window_word: None,
            moving: false,
            area: leviculum_ui::Rect::new(1, 1, cols, rows),
            viewport: view,
            cell_px: (1, 2),
            pins: Vec::new(),
        };
        let mut terminal = Terminal::new(TestBackend::new(cols + 2, rows + 2)).expect("terminal");
        let Ok(_) = terminal.draw(|f| painter.render(f, &shown, Style::default()));
        let buffer = terminal.backend().buffer().clone();
        // The bottom row carries the attribution; the rows above it are
        // the picture.
        (1..=cols)
            .map(|x| {
                (1..rows)
                    .filter_map(|y| buffer.cell((x, y)).map(|c| (c.fg, c.bg)))
                    .collect()
            })
            .collect()
    }

    /// The terminal shows one world at most (1131): a pane 300 pixels wide
    /// asked for z0, where the world is 256 wide, is drawn at z1 by the
    /// model's view, and no column of its ground repeats. The view as
    /// asked, the control, repeats every 256 columns. The names are off:
    /// they are placed per view (1683), not per copy of the world.
    #[test]
    fn the_terminal_never_shows_the_world_twice() {
        let setup = MapSetup {
            cell_px: (1, 2),
            ..MapSetup::default()
        };
        let area = leviculum_ui::Rect::new(1, 1, 300, 60);
        let state = loquax::ui::map::MapState {
            centre: project(LatLon::from_degrees(30.0, 10.0)),
            zoom: 0,
            selected: None,
        };
        let asked = Viewport::new(state.centre, 0, 300, 120);
        assert_eq!(period(&halfblock_columns(asked)), Some(256), "the control");
        let view = loquax::ui::map::viewport(&state, area, &setup);
        assert_eq!(view.zoom(), 1);
        assert_eq!(period(&halfblock_columns(view)), None);
    }

    /// The names of the Florence fixture over its picture in braille, with
    /// a pin: the park names the view's zoom shows set by the density
    /// rule, each with its cells' background as its halo at truecolor, none
    /// over the pin; the frame pinned as text.
    #[test]
    fn the_map_names_its_places_by_the_density_rule() {
        let dir =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../leviculum-map/fixtures");
        let mut painter = MapPainter::new(Maps::new(Some(dir)), Graphics::Braille, None);
        painter.set_tier(Tier::Truecolor);
        let cell_px = painter.setup().cell_px;
        let (cols, rows) = (60u16, 20u16);
        let area = leviculum_ui::Rect::new(0, 0, cols, rows);
        let at = project(LatLon::from_degrees(43.7755, 11.2672));
        let shown = MapView {
            area,
            viewport: Viewport::new(
                at,
                15,
                u32::from(cols) * u32::from(cell_px.0),
                u32::from(rows) * u32::from(cell_px.1),
            ),
            cell_px,
            tracks: Vec::new(),
            radar: None,
            window_word: None,
            moving: false,
            pins: vec![PinCell {
                peer: [1; 16],
                col: 2,
                row: 1,
                text: "● bob · 4 min".to_string(),
                apart: None,
                stale: false,
                nearby: false,
                selected: false,
                own: false,
            }],
        };
        let mut terminal = Terminal::new(TestBackend::new(cols, rows + 1)).expect("terminal");
        let Ok(_) = terminal.draw(|f| painter.render(f, &shown, Style::default()));
        let buffer = terminal.backend().buffer().clone();
        let text: Vec<String> = (0..rows)
            .map(|y| {
                (0..cols)
                    .map(|x| match buffer.cell((x, y)).map(|c| c.symbol()) {
                        Some(s) if s.chars().all(|c| c.is_alphanumeric() || " '·●".contains(c)) => {
                            s.to_string()
                        }
                        _ => " ".to_string(),
                    })
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect();
        let frame = text.join("\n");
        assert!(frame.contains("● bob · 4 min"), "{frame}");
        assert!(frame.contains("Piazza D'Azeglio"), "{frame}");
        let row = text.iter().position(|l| l.contains("Piazza")).expect("row") as u16;
        let col = text[usize::from(row)].find("Piazza").expect("col") as u16;
        let cell = buffer.cell((col, row)).expect("cell");
        assert_eq!(cell.bg, Color::Rgb(0xff, 0xff, 0xff), "the halo");
        assert_eq!(cell.fg, Color::Rgb(0x2e, 0x6b, 0x2e), "a park's green");
        assert_eq!(frame, PINNED_NAMES, "{frame}");
    }

    /// 1422: a drag is the frame budget's gesture. At the minimal rung a
    /// frame while the pointer drags leaves the map's names out, as the
    /// pixel screens let them ride; the frame as the button comes up puts
    /// them back. At the full rung the drag keeps them.
    #[test]
    fn a_drag_at_the_minimal_rung_leaves_the_names_to_the_rest() {
        let dir =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../leviculum-map/fixtures");
        let mut painter = MapPainter::new(Maps::new(Some(dir)), Graphics::Braille, None);
        let cell_px = painter.setup().cell_px;
        let (cols, rows) = (60u16, 20u16);
        let at = project(LatLon::from_degrees(43.7755, 11.2672));
        let shown = |moving: bool| MapView {
            area: leviculum_ui::Rect::new(0, 0, cols, rows),
            viewport: Viewport::new(
                at,
                15,
                u32::from(cols) * u32::from(cell_px.0),
                u32::from(rows) * u32::from(cell_px.1),
            ),
            cell_px,
            tracks: Vec::new(),
            radar: None,
            window_word: None,
            moving,
            pins: Vec::new(),
        };
        let mut terminal = Terminal::new(TestBackend::new(cols, rows + 1)).expect("terminal");
        let mut text = |painter: &mut MapPainter, moving: bool| {
            let Ok(_) = terminal.draw(|f| painter.render(f, &shown(moving), Style::default()));
            let buffer = terminal.backend().buffer().clone();
            (0..rows)
                .flat_map(|y| (0..cols).map(move |x| (x, y)))
                .filter_map(|(x, y)| buffer.cell((x, y)).map(|c| c.symbol().to_string()))
                .collect::<String>()
        };
        painter.hold_rung(Some(2));
        assert!(!text(&mut painter, true).contains("Piazza"), "riding");
        assert!(painter.ladder.moving());
        assert!(text(&mut painter, false).contains("Piazza"), "at rest");
        assert!(!painter.ladder.moving());
        painter.hold_rung(Some(0));
        assert!(text(&mut painter, true).contains("Piazza"), "the full rung");
    }

    /// 1302: a node the mesh shows that is no contact is drawn beside a
    /// friend's pin in the muted grey, its announced name beside it, the
    /// friend's in the plain text colour; the frame pinned as text.
    #[test]
    fn a_node_nearby_is_drawn_grey_beside_a_friends_pin() {
        let dir =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../leviculum-map/fixtures");
        let mut painter = MapPainter::new(Maps::new(Some(dir)), Graphics::Braille, None);
        painter.set_tier(Tier::Truecolor);
        let cell_px = painter.setup().cell_px;
        let (cols, rows) = (60u16, 20u16);
        let area = leviculum_ui::Rect::new(0, 0, cols, rows);
        let at = project(LatLon::from_degrees(43.7755, 11.2672));
        let pin = |peer: u8, row: u16, text: &str, nearby: bool| PinCell {
            peer: [peer; 16],
            col: 2,
            row,
            text: text.to_string(),
            apart: None,
            stale: false,
            nearby,
            selected: false,
            own: false,
        };
        let shown = MapView {
            tracks: Vec::new(),
            radar: None,
            window_word: None,
            moving: false,
            area,
            viewport: Viewport::new(
                at,
                15,
                u32::from(cols) * u32::from(cell_px.0),
                u32::from(rows) * u32::from(cell_px.1),
            ),
            cell_px,
            pins: vec![
                pin(1, 1, "● bob · 4 min", false),
                pin(2, 4, "● Wanderer ·353e · 2 min", true),
            ],
        };
        let grey = Color::Rgb(0x80, 0x80, 0x80);
        let muted = Style::default().fg(grey);
        let mut terminal = Terminal::new(TestBackend::new(cols, rows + 1)).expect("terminal");
        let Ok(_) = terminal.draw(|f| painter.render(f, &shown, muted));
        let buffer = terminal.backend().buffer().clone();
        let line = |y: u16, from: u16, len: u16| -> String {
            (from..from + len)
                .map(|x| buffer.cell((x, y)).map_or(" ", |c| c.symbol()).to_string())
                .collect()
        };
        assert_eq!(line(1, 2, 13), "● bob · 4 min");
        assert_eq!(line(4, 2, 24), "● Wanderer ·353e · 2 min");
        let fg = |x: u16, y: u16| buffer.cell((x, y)).expect("cell").fg;
        assert_ne!(fg(4, 1), grey, "a friend's name is no grey");
        for x in 2..26 {
            assert_eq!(fg(x, 4), grey, "the node nearby is grey at column {x}");
        }
    }

    const PINNED_NAMES: &str = "\n  ● bob · 4 min  Giardino del Borgo\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n                           Piazza D'Azeglio\n";

    /// The streets of the Florence fixture named in cells (1126): at z16
    /// by the Duomo over its picture in braille, each name on a level run of its street
    /// two cells longer than the name, one name a 20 x 6 block, a free cell
    /// around it, none over the pin; the frame pinned as text, and the
    /// styles of a street's cells the places' halo without bold.
    #[test]
    fn the_map_names_its_streets_on_level_runs() {
        let dir =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../leviculum-map/fixtures");
        let mut painter = MapPainter::new(Maps::new(Some(dir)), Graphics::Braille, None);
        painter.set_tier(Tier::Truecolor);
        let cell_px = painter.setup().cell_px;
        let (cols, rows) = (80u16, 24u16);
        let area = leviculum_ui::Rect::new(0, 0, cols, rows);
        let at = project(LatLon::from_degrees(43.7737, 11.2526));
        let shown = MapView {
            area,
            viewport: Viewport::new(
                at,
                16,
                u32::from(cols) * u32::from(cell_px.0),
                u32::from(rows) * u32::from(cell_px.1),
            ),
            cell_px,
            tracks: Vec::new(),
            radar: None,
            window_word: None,
            moving: false,
            pins: vec![PinCell {
                peer: [1; 16],
                col: 2,
                row: 1,
                text: "● bob · 4 min".to_string(),
                apart: None,
                stale: false,
                nearby: false,
                selected: false,
                own: false,
            }],
        };
        let mut terminal = Terminal::new(TestBackend::new(cols, rows + 1)).expect("terminal");
        let Ok(_) = terminal.draw(|f| painter.render(f, &shown, Style::default()));
        let buffer = terminal.backend().buffer().clone();
        let text: Vec<String> = (0..rows)
            .map(|y| {
                (0..cols)
                    .map(|x| match buffer.cell((x, y)).map(|c| c.symbol()) {
                        Some(s) if s.chars().all(|c| c.is_alphanumeric() || " '·●".contains(c)) => {
                            s.to_string()
                        }
                        _ => " ".to_string(),
                    })
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect();
        let frame = text.join("\n");
        let streets = frame.matches("Via ").count();
        assert!(streets >= 2, "{frame}");
        let row = text.iter().position(|l| l.contains("Via ")).expect("row") as u16;
        let col = text[usize::from(row)].find("Via ").expect("col") as u16;
        let cell = buffer.cell((col, row)).expect("cell");
        assert_eq!(cell.bg, Color::Rgb(0xff, 0xff, 0xff), "the halo");
        assert_eq!(cell.fg, Color::Rgb(0x20, 0x20, 0x20), "a street's ink");
        assert!(!cell.modifier.contains(Modifier::BOLD));
        assert_eq!(frame, PINNED_STREETS, "{frame}");
    }

    const PINNED_STREETS: &str = "\n  ● bob · 4 min\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n                Via dei Banchi            Via dei Cerretani\n\n\n\n\n\n\n";

    /// The text techniques of the map core keep to their glyphs: octants
    /// to the octant table, `blocks` to quadrants and eighths, braille-thin
    /// to braille, mosaic to blanks; each cell in two colours of RGB at
    /// truecolor, and at 16 colours braille-thin and mosaic in the class's
    /// own of the sixteen, never an RGB a 16-colour terminal cannot show.
    #[test]
    fn the_text_techniques_keep_to_their_glyphs_and_colour_tiers() {
        let octants: Vec<char> = cells::OCTANTS.to_vec();
        let blocks: Vec<char> = cells::blocks().iter().map(|g| g.ch).collect();
        for graphics in [
            Graphics::Octant,
            Graphics::Blocks,
            Graphics::BrailleThin,
            Graphics::Mosaic,
        ] {
            for tier in [Tier::Truecolor, Tier::Ansi16] {
                let mut painter = MapPainter::new(Maps::default(), graphics, None);
                painter.set_tier(tier);
                let shown = shown(painter.setup().cell_px);
                let mut terminal = Terminal::new(TestBackend::new(32, 10)).expect("terminal");
                terminal
                    .draw(|f| painter.render(f, &shown, Style::default()))
                    .expect("draw");
                let buffer = terminal.backend().buffer().clone();
                let mut picture = 0;
                let mut inked = 0;
                let mut named = 0;
                // The picture's cells: inside (1, 1) 30 x 8, off the pin's
                // row, whose label covers part of it.
                for y in 1..9 {
                    for x in 1..31 {
                        if y == 5 {
                            continue;
                        }
                        let cell = buffer.cell((x, y)).expect("cell");
                        let ch = cell.symbol().chars().next().unwrap_or(' ');
                        if ch.is_alphanumeric() {
                            // A name's letter, not the technique's glyph
                            // (1683): the label tests measure those.
                            named += 1;
                            continue;
                        }
                        let ok = match graphics {
                            Graphics::Octant => octants.contains(&ch),
                            Graphics::Blocks => blocks.contains(&ch),
                            Graphics::BrailleThin => {
                                ch == ' ' || ('\u{2801}'..='\u{28FF}').contains(&ch)
                            }
                            _ => ch == ' ',
                        };
                        assert!(ok, "{graphics:?}: {ch:?} at ({x}, {y})");
                        picture += 1;
                        if ch != ' ' {
                            inked += 1;
                        }
                        match (tier, graphics) {
                            (Tier::Truecolor, _) => {
                                assert!(
                                    matches!(cell.bg, Color::Rgb(..)),
                                    "{graphics:?}: {cell:?}"
                                );
                            }
                            (_, Graphics::BrailleThin | Graphics::Mosaic) => {
                                assert!(
                                    matches!(cell.bg, Color::Indexed(i) if i < 16),
                                    "{graphics:?}: {cell:?}"
                                );
                                if ch != ' ' {
                                    assert!(
                                        matches!(cell.fg, Color::Indexed(i) if i < 16),
                                        "{graphics:?}: {cell:?}"
                                    );
                                }
                            }
                            _ => {}
                        }
                    }
                }
                assert_eq!(picture + named, 30 * 7);
                // The coasts of Europe at z1 are drawn: some cell carries a
                // glyph, except in the mosaic, which has none.
                match graphics {
                    Graphics::Mosaic => assert_eq!(inked, 0),
                    _ => assert!(inked > 0, "{graphics:?}: nothing drawn"),
                }
                let text: String = buffer.content().iter().map(|c| c.symbol()).collect();
                assert!(text.contains("● bob · 4 min"), "{graphics:?}");
                assert!(text.contains("Natural Earth"), "{graphics:?}");
            }
        }
    }

    /// At 16 colours the class colours reach the terminal as `38;5;n` and
    /// `48;5;n` with n under 16, at 256 as palette indexes: no 24-bit SGR.
    #[test]
    fn braille_thin_at_16_and_256_colours_emits_no_24_bit_sgr() {
        use leviculum_ui::color::ColorDepth;
        use leviculum_ui_ratatui::style::downgrade_buffer;
        use ratatui::backend::CrosstermBackend;
        #[derive(Clone, Default)]
        struct Shared(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);
        impl std::io::Write for Shared {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.borrow_mut().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        for (tier, depth) in [
            (Tier::Ansi16, ColorDepth::Ansi16),
            (Tier::Ansi256, ColorDepth::Ansi256),
        ] {
            let mut painter = MapPainter::new(Maps::default(), Graphics::BrailleThin, None);
            painter.set_tier(tier);
            let shown = shown(painter.setup().cell_px);
            let bytes = Shared::default();
            let options = ratatui::TerminalOptions {
                viewport: ratatui::Viewport::Fixed(Rect::new(0, 0, 32, 10)),
            };
            let mut terminal =
                Terminal::with_options(CrosstermBackend::new(bytes.clone()), options)
                    .expect("terminal");
            terminal
                .draw(|f| {
                    painter.render(f, &shown, Style::default());
                    downgrade_buffer(f, depth, false);
                })
                .expect("draw");
            let out = String::from_utf8_lossy(&bytes.0.borrow()).into_owned();
            let sgr: Vec<&str> = out
                .split("\u{1b}[")
                .skip(1)
                .filter_map(|s| s.split_once('m').map(|(p, _)| p))
                .filter(|p| p.chars().all(|c| c.is_ascii_digit() || c == ';'))
                .collect();
            assert!(sgr.iter().any(|p| p.contains("48;5;")), "{tier:?}: {out:?}");
            for p in &sgr {
                assert!(
                    !p.contains("38;2") && !p.contains("48;2"),
                    "{tier:?}: 24-bit SGR {p:?}"
                );
                if tier == Tier::Ansi16 {
                    for (at, _) in p.match_indices("8;5;") {
                        let n: u32 = p[at + 4..]
                            .split(';')
                            .next()
                            .and_then(|n| n.parse().ok())
                            .expect("an index");
                        assert!(n < 16, "{p:?}");
                    }
                }
            }
        }
    }

    /// The octant fit's cost over Lew's pane, 155 x 65 cells of the
    /// Florence fixture at z15, against the brute force over the 256
    /// patterns that the draft measured at 34 ms: the same cells, and the
    /// time of each printed. In the suite on a small pane, where only the
    /// equality is checked; `LOQUAX_FRAME_REGION` set (as for
    /// [`frame_cost`]) measures the full pane:
    /// `LOQUAX_FRAME_REGION=x cargo test --release -p loquax --features tui
    /// --bins octant_cost -- --nocapture`.
    #[test]
    fn octant_cost() {
        let region =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../leviculum-map/fixtures/firenze.lmr");
        let dir = tempfile::tempdir().expect("maps dir");
        std::os::unix::fs::symlink(
            std::fs::canonicalize(&region).expect("region"),
            dir.path().join("f.lmr"),
        )
        .expect("link");
        let (cols, rows) = match measure_on() {
            true => (155u16, 65u16),
            false => (30, 10),
        };
        let mut canvas = Canvas {
            painter: Painter::new(Maps::new(Some(dir.path().to_path_buf()))),
            graphics: Graphics::Octant,
            pictures: Pictures::new(None, Backend::Text),
            tmux: false,
            sets: Sets::default(),
        };
        let view = Viewport::new(
            project(LatLon::from_degrees(43.7731, 11.2560)),
            14,
            u32::from(cols) * 2,
            u32::from(rows) * 4,
        );
        let t0 = Instant::now();
        let (source, _) = canvas.source(&view, (cols, rows), Encoding::Octant, MapStyle::PC);
        let drawn = t0.elapsed();
        let octants = cells::octants();
        let n = if measure_on() { 5 } else { 1 };
        let (mut fast, mut brute) = (Vec::new(), Vec::new());
        let (mut by_fit, mut by_brute) = (Vec::new(), Vec::new());
        for _ in 0..n {
            let t = Instant::now();
            by_fit = cells::encode(&source, Encoding::Octant, &canvas.sets);
            fast.push(t.elapsed());
            let t = Instant::now();
            by_brute = (0..source.rows)
                .flat_map(|row| (0..source.cols).map(move |col| (col, row)))
                .map(|(col, row)| cells::fit(&source.block(col, row), &octants))
                .collect();
            brute.push(t.elapsed());
        }
        assert_eq!(by_fit, by_brute);
        assert!(by_fit.iter().any(|c| c.fg != c.bg), "the streets are drawn");
        let median = |mut v: Vec<Duration>| {
            v.sort();
            v[v.len() / 2].as_secs_f64() * 1e3
        };
        println!(
            "OCTANT cells={cols}x{rows} source_ms={:.1} fit_ms={:.2} brute_force_ms={:.2}",
            drawn.as_secs_f64() * 1e3,
            median(fast),
            median(brute)
        );
    }

    /// What a terminal shows after the frames ratatui sent it: each
    /// frame's diff against the one before applied in order, as
    /// `Terminal::flush` sends it. A sixel's leading erase is carried out,
    /// `ECH` of its width on each of its rows, and the picture's cells are
    /// `▒`; a kitty row writes its placeholders across the row, also `▒`.
    /// A `TestBackend` alone keeps text an erase would remove.
    struct Screen {
        shown: ratatui::buffer::Buffer,
        last: ratatui::buffer::Buffer,
    }

    impl Screen {
        fn new(area: Rect) -> Self {
            Self {
                shown: ratatui::buffer::Buffer::empty(area),
                last: ratatui::buffer::Buffer::empty(area),
            }
        }

        fn frame(
            &mut self,
            terminal: &mut Terminal<TestBackend>,
            painter: &mut MapPainter,
            shown: &MapView,
        ) {
            let next = terminal
                .draw(|f| painter.render(f, shown, Style::default()))
                .expect("draw")
                .buffer
                .clone();
            let updates: Vec<(u16, u16, ratatui::buffer::Cell)> = self
                .last
                .diff(&next)
                .into_iter()
                .map(|(x, y, cell)| (x, y, cell.clone()))
                .collect();
            for (x, y, cell) in updates {
                self.write(x, y, &cell);
            }
            self.last = next;
        }

        fn write(&mut self, x: u16, y: u16, cell: &ratatui::buffer::Cell) {
            let symbol = cell.symbol();
            let mut picture = |x: u16, y: u16| {
                if let Some(c) = self.shown.cell_mut((x, y)) {
                    c.reset();
                    c.set_symbol("▒");
                }
            };
            // Kitty's placeholders first: behind tmux its transmission is
            // wrapped in a passthrough DCS, which is no sixel. ratatui-image
            // reads tmux from the environment for every picker, so a test
            // run from a tmux pane gets the wrapped form (1347).
            if symbol.contains('\u{10EEEE}') {
                let width = symbol.chars().filter(|&c| c == '\u{10EEEE}').count() as u16;
                for dx in 0..width {
                    picture(x + dx, y);
                }
            } else if symbol.contains("\u{1b}P") {
                let width: u16 = symbol
                    .split_once("\u{1b}[")
                    .and_then(|(_, rest)| rest.split_once('X'))
                    .and_then(|(n, _)| n.parse().ok())
                    .expect("a sixel opens with an erase");
                let rows = symbol.matches("X\u{1b}[1B").count().max(1) as u16;
                for dy in 0..rows {
                    for dx in 0..width {
                        picture(x + dx, y + dy);
                    }
                }
            } else if let Some(c) = self.shown.cell_mut((x, y)) {
                *c = cell.clone();
            }
        }

        fn row(&self, y: u16) -> String {
            (0..self.shown.area.width)
                .filter_map(|x| self.shown.cell((x, y)).map(|c| c.symbol().to_string()))
                .collect()
        }
    }

    /// A 30 x 8 picture at (1, 1) of `centre` at `zoom`, with one pin at
    /// `(col, 5)`.
    fn one_pin(painter: &MapPainter, centre: World, zoom: u8, col: u16, text: &str) -> MapView {
        let cell_px = painter.setup().cell_px;
        let mut view = shown(cell_px);
        view.viewport = Viewport::new(
            centre,
            zoom,
            30 * u32::from(cell_px.0),
            8 * u32::from(cell_px.1),
        );
        view.pins[0].col = col;
        view.pins[0].text = text.to_string();
        view.pins[0].selected = false;
        view
    }

    fn graphics_painters() -> [MapPainter; 2] {
        [
            MapPainter::new(Maps::default(), Graphics::Sixel, Some(Picker::halfblocks())),
            MapPainter::new(Maps::default(), Graphics::Kitty, None),
        ]
    }

    /// A drag over open sea: the picture is the same plain colour before
    /// and after, so it is not sent again, and the label moved three cells
    /// left. Its old cells must not keep the old text (1074, Lew's foot:
    /// two labels of one peer).
    #[test]
    fn a_label_dragged_over_an_unchanged_picture_leaves_no_old_cells() {
        let sea = project(LatLon::from_degrees(40.0, -40.0));
        for mut painter in graphics_painters() {
            let name = painter.graphics().name();
            let (cw, _) = painter.setup().cell_px;
            let mut terminal = Terminal::new(TestBackend::new(32, 10)).expect("terminal");
            let mut screen = Screen::new(Rect::new(0, 0, 32, 10));
            let before = one_pin(&painter, sea, 8, 16, "● bob · 4 min");
            screen.frame(&mut terminal, &mut painter, &before);
            assert_eq!(screen.row(5).matches("bob").count(), 1, "{name}");
            let mut dragged = before.viewport;
            dragged.pan(3 * i32::from(cw), 0);
            let after = one_pin(&painter, dragged.centre, 8, 13, "● bob · 4 min");
            screen.frame(&mut terminal, &mut painter, &after);
            let row = screen.row(5);
            assert_eq!(row.matches("bob").count(), 1, "{name}: {row}");
            assert_eq!(row.matches("min").count(), 1, "{name}: {row}");
            assert!(row.contains("▒● bob · 4 min▒"), "{name}: {row}");
        }
    }

    /// The age shortens over a view that did not move: the picture is the
    /// one drawn last, and the cell the longer text took must not keep its
    /// last letter.
    #[test]
    fn a_label_that_shortens_over_the_same_view_leaves_no_old_cells() {
        let at = project(LatLon::from_degrees(48.0, 10.0));
        for mut painter in graphics_painters() {
            let name = painter.graphics().name();
            let mut terminal = Terminal::new(TestBackend::new(32, 10)).expect("terminal");
            let mut screen = Screen::new(Rect::new(0, 0, 32, 10));
            let long = one_pin(&painter, at, 1, 16, "● bob · 14 min");
            screen.frame(&mut terminal, &mut painter, &long);
            let short = one_pin(&painter, at, 1, 16, "● bob · 4 min");
            screen.frame(&mut terminal, &mut painter, &short);
            let row = screen.row(5);
            assert!(row.contains("▒● bob · 4 min▒"), "{name}: {row}");
        }
    }

    /// A zoom about the pin: the picture changes and is sent again, which
    /// erases what was under it, and the label stays on the same cells. It
    /// must be drawn again over the new picture.
    #[test]
    fn a_label_on_the_same_cells_over_a_new_picture_is_drawn_again() {
        let at = project(LatLon::from_degrees(48.0, 10.0));
        for mut painter in graphics_painters() {
            let name = painter.graphics().name();
            let mut terminal = Terminal::new(TestBackend::new(32, 10)).expect("terminal");
            let mut screen = Screen::new(Rect::new(0, 0, 32, 10));
            let out = one_pin(&painter, at, 1, 16, "● bob · 4 min");
            screen.frame(&mut terminal, &mut painter, &out);
            let zoomed = one_pin(&painter, at, 2, 16, "● bob · 4 min");
            screen.frame(&mut terminal, &mut painter, &zoomed);
            let row = screen.row(5);
            assert!(row.contains("▒● bob · 4 min▒"), "{name}: {row}");
        }
    }

    /// The cost of one keyboard step, phase by phase, the way ratatui-image
    /// encoded it before 1075 and the way the painter does now.
    ///
    /// In the suite: the Florence fixture on a small pane, checking that a
    /// map frame keeps to the few colours the painter's own sixel needs,
    /// and that its sixel is no larger than ratatui-image's. A style that
    /// grew past 256 colours (shading, smoothing) would fall back to the
    /// quantiser silently, and the frame back to five times its cost.
    ///
    /// With `LOQUAX_FRAME_REGION` set to a region file: Lew's pane (foot,
    /// 9 x 19 cells, 155 x 65 of them) over Frankfurt at z11, z14 and z17,
    /// and a held key at 25 and 50 repeats a second against both ways.
    /// A measurement, printed: `LOQUAX_FRAME_REGION=frankfurt.lmr cargo
    /// test --release -p loquax --features tui --bins frame_cost --
    /// --nocapture`.
    #[test]
    fn frame_cost() {
        let measure = std::env::var_os("LOQUAX_FRAME_REGION");
        let path = measure.clone().map_or_else(
            || {
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../leviculum-map/fixtures/firenze.lmr")
            },
            PathBuf::from,
        );
        // The region alone in a maps directory of its own, linked.
        let dir = tempfile::tempdir().expect("maps dir");
        let linked = std::fs::canonicalize(&path).expect("region");
        std::os::unix::fs::symlink(&linked, dir.path().join("region.lmr")).expect("link");
        let open = || Maps::new(Some(dir.path().to_path_buf()));
        let ((cols, rows), at, zooms): ((u16, u16), _, &[u8]) = match measure {
            Some(_) => (
                (155, 65),
                project(LatLon::from_degrees(50.1106, 8.6821)),
                &[11, 14, 17],
            ),
            None => (
                (40, 12),
                project(LatLon::from_degrees(43.7731, 11.2560)),
                &[13],
            ),
        };
        let font = FontSize {
            width: 9,
            height: 19,
        };
        let (w, h) = (u32::from(cols) * 9, u32::from(rows) * 19);
        let picker = || {
            // Deprecated for a program, which asks the terminal; the
            // measurement needs foot's 9 x 19 without one.
            #[allow(deprecated)]
            let mut picker = Picker::from_fontsize(font);
            picker.set_protocol_type(ProtocolType::Sixel);
            picker
        };
        let ms = |d: Duration| d.as_secs_f64() * 1e3;
        let bytes_of = |render: &dyn Fn(&mut TermFrame)| -> usize {
            let mut terminal = Terminal::new(TestBackend::new(cols, rows)).expect("terminal");
            let Ok(_) = terminal.draw(render);
            let buffer = terminal.backend().buffer();
            buffer.content().iter().map(|c| c.symbol().len()).sum()
        };
        let mut canvas = Canvas {
            painter: Painter::new(open()),
            graphics: Graphics::Sixel,
            pictures: Pictures::new(Some(picker()), Backend::Graphics(ProtocolType::Sixel)),
            tmux: false,
            sets: Sets::default(),
        };
        let mut read = Vec::new();
        let area = Rect::new(0, 0, cols, rows);
        let n = if measure_on() { 5 } else { 1 };
        for &zoom in zooms {
            let mut sums = [0f64; 6];
            let (mut before, mut after) = (0, 0);
            for i in 0..n {
                // A step east each round, so no round redraws the last.
                let mut view = Viewport::new(at, zoom, w, h);
                view.pan(i * (w as i32 / 8), 0);
                let mut fb = vec![0u16; (w * h) as usize];
                let t0 = Instant::now();
                canvas.painter.regions()[0]
                    .region
                    .read_view(&view, &mut read)
                    .expect("read");
                let t1 = Instant::now();
                let ask = Ask {
                    view,
                    cells: (cols, rows),
                    epoch: 0,
                    style: MapStyle::M9,
                    time_zones: None,
                };
                // Painted whole for the render's share: the picture is
                // encoded again below, both ways.
                let graphics = canvas.graphics;
                canvas.graphics = Graphics::Halfblocks;
                let painted = canvas.paint(ask);
                canvas.graphics = graphics;
                fb.copy_from_slice(&painted.fb);
                let t2 = Instant::now();
                let mut rgb = Vec::with_capacity(fb.len() * 3);
                if let Some(f) = Frame::new(&mut fb, w, h) {
                    f.rgb888_into(&mut rgb);
                }
                let t3 = Instant::now();
                let image = images::rgb_image(w, h, rgb).expect("image");
                let old = canvas.pictures.encode(image, (cols, rows)).expect("encode");
                let t4 = Instant::now();
                let (palette, indices) = indexed(&fb).expect("at most 256 colours");
                let t5 = Instant::now();
                let new = images::sixel_indexed_direct(&palette, &indices, w, h, (cols, rows))
                    .expect("sixel");
                let t6 = Instant::now();
                before = bytes_of(&|f| images::render(f, area, &old, 0, 0));
                after = bytes_of(&|f| images::render_whole(f, area, &new));
                // The render reads the view again, warm: its read is taken
                // off it.
                let read = ms(t1 - t0);
                let phases = [
                    read,
                    ms(t2 - t1) - read,
                    ms(t3 - t2),
                    ms(t4 - t3),
                    ms(t5 - t4),
                    ms(t6 - t5),
                ];
                for (s, d) in sums.iter_mut().zip(phases) {
                    *s += d;
                }
            }
            assert!(after <= before, "z{zoom}: {after} bytes against {before}");
            let [read, render, rgb, quantised, index, indexed] = sums.map(|s| s / f64::from(n));
            println!(
                "FRAME zoom={zoom} px={w}x{h} read_ms={read:.1} render_ms={render:.1} \
                 before: rgb_ms={rgb:.1} encode_ms={quantised:.1} total_ms={:.1} bytes={before} \
                 after: index_ms={index:.1} encode_ms={indexed:.1} total_ms={:.1} bytes={after}",
                read + render + rgb + quantised,
                read + render + index + indexed,
            );
        }
        if !measure_on() {
            return;
        }
        // A held arrow for two seconds at z14, at foot's default repeat of
        // 25 a second and at 50. Before: every key a frame in turn on the
        // input path, encoded by ratatui-image. After: the painter on its
        // thread, each frame asking for the newest view.
        let step = |i: u32| {
            let mut view = Viewport::new(at, 14, w, h);
            view.pan(i as i32 * (w as i32 / 8), 0);
            Ask {
                view,
                cells: (cols, rows),
                epoch: 0,
                style: MapStyle::M9,
                time_zones: None,
            }
        };
        for per_s in [25u32, 50] {
            let repeat = Duration::from_millis(u64::from(1000 / per_s));
            let keys = 2 * per_s;
            let start = Instant::now();
            let mut done = start;
            for i in 0..keys {
                if let Some(wait) = (start + repeat * i).checked_duration_since(Instant::now()) {
                    std::thread::sleep(wait);
                }
                let graphics = canvas.graphics;
                canvas.graphics = Graphics::Halfblocks;
                let mut painted = canvas.paint(step(i));
                canvas.graphics = graphics;
                let mut rgb = Vec::with_capacity(painted.fb.len() * 3);
                if let Some(f) = Frame::new(&mut painted.fb, w, h) {
                    f.rgb888_into(&mut rgb);
                }
                let image = images::rgb_image(w, h, rgb).expect("image");
                let _ = canvas.pictures.encode(image, (cols, rows)).expect("encode");
                done = Instant::now();
            }
            let last = start + repeat * (keys - 1);
            println!(
                "HELD before per_s={per_s} keys={keys} frames={keys} frames_per_s={:.1} \
                 lag_after_release_ms={:.0}",
                f64::from(keys) / (done - start).as_secs_f64(),
                ms(done - last)
            );
            let mut threaded = MapPainter::new(open(), Graphics::Sixel, Some(picker())).threaded();
            let start = Instant::now();
            let mut frames = 0u32;
            let mut shown = None;
            let mut frame = |painter: &mut MapPainter, ask: Ask| {
                painter.paint(ask, Instant::now());
                let now = painter.painted.as_ref().map(|p| p.view);
                if now != shown {
                    shown = now;
                    frames += 1;
                }
            };
            for i in 0..keys {
                // The loop draws when the painter wakes it: a frame a
                // millisecond stands for that, at the view of the last key.
                while Instant::now() < start + repeat * i {
                    frame(&mut threaded, step(i.saturating_sub(1)));
                    std::thread::sleep(Duration::from_millis(1));
                }
                frame(&mut threaded, step(i));
            }
            let last = start + repeat * (keys - 1);
            let wanted = step(keys - 1);
            while threaded.painted.as_ref().map(|p| p.view) != Some(wanted.view) {
                std::thread::sleep(Duration::from_millis(1));
                frame(&mut threaded, wanted);
            }
            let caught = Instant::now();
            println!(
                "HELD after per_s={per_s} keys={keys} frames={frames} frames_per_s={:.1} \
                 lag_after_release_ms={:.0}",
                f64::from(frames) / (caught - start).as_secs_f64(),
                ms(caught - last)
            );
        }
    }

    /// The frame's colours become the sixel's palette in the order they
    /// first appear; past 256 there is no palette, and ratatui-image's
    /// quantiser draws the frame instead.
    #[test]
    fn a_frame_is_indexed_by_its_colours_up_to_256() {
        let (palette, indices) = indexed(&[7, 9, 7, 0]).expect("three colours");
        assert_eq!(palette.len(), 3);
        assert_eq!(indices, [0, 1, 0, 2]);
        let full: Vec<u16> = (0..256).collect();
        assert_eq!(indexed(&full).map(|(p, _)| p.len()), Some(256));
        let over: Vec<u16> = (0..257).collect();
        assert!(indexed(&over).is_none());
    }

    /// Whether `frame_cost` measures Lew's pane rather than checks the
    /// fixture.
    fn measure_on() -> bool {
        std::env::var_os("LOQUAX_FRAME_REGION").is_some()
    }

    /// A view of the outline over Europe, 30 x 8 cells of braille, `i`
    /// eighths east.
    fn ask_east(i: i32) -> Ask {
        let mut view = Viewport::new(project(LatLon::from_degrees(48.0, 10.0)), 4, 60, 32);
        view.pan(i * 60 / 8, 0);
        Ask {
            view,
            cells: (30, 8),
            epoch: 0,
            style: MapStyle::M9,
            time_zones: None,
        }
    }

    /// The paint of `ask` as the thread would finish it.
    fn painted(ask: Ask) -> Painted {
        Painted {
            view: ask.view,
            cells: ask.cells,
            style: ask.style,
            epoch: ask.epoch,
            time_zones: ask.time_zones,
            fb: Vec::new(),
            attribution: String::new(),
            picture: None,
            glyphs: Vec::new(),
            names: Vec::new(),
            paint_us: 0,
        }
    }

    /// The newest view wins (1075): views asked while a paint runs replace
    /// each other, so only the last is painted next; a paint that finishes
    /// for a view the reader has moved on from is dropped, unless the
    /// picture on screen has stood for [`CATCH_UP_MS`], and then it is the
    /// nearest there is.
    #[test]
    fn a_paint_overtaken_by_a_newer_view_is_dropped() {
        let (a, b, c) = (ask_east(1), ask_east(2), ask_east(3));
        let mut slot = Slot::default();
        slot.ask(a);
        assert_eq!(slot.start(), Some(a), "the thread starts A");
        slot.ask(b);
        slot.ask(c);
        slot.finish(painted(a));
        // A finished for a view behind C, with a picture shown just now.
        let fresh = Some(Duration::from_millis(10));
        let done = slot.done.take().expect("A is done");
        let of = Ask {
            view: done.view,
            cells: done.cells,
            epoch: done.epoch,
            style: done.style,
            time_zones: done.time_zones,
        };
        assert!(!keep(&of, &c, fresh), "A is dropped");
        assert_eq!(slot.start(), Some(c), "B is never painted");
        assert_eq!(slot.start(), None);
        slot.finish(painted(c));
        let done = slot.done.take().expect("C is done");
        let of = Ask {
            view: done.view,
            cells: done.cells,
            epoch: done.epoch,
            style: done.style,
            time_zones: done.time_zones,
        };
        assert!(keep(&of, &c, fresh), "C is the view wanted");
        // Keys faster than paints: after CATCH_UP_MS the paint behind is
        // shown, and so is any first picture.
        let stood = Some(Duration::from_millis(CATCH_UP_MS));
        assert!(keep(&ask_east(1), &c, stood));
        assert!(keep(&ask_east(1), &c, None));
        // Same view, other cells: a resize is another paint.
        let mut wider = c;
        wider.cells.0 += 1;
        assert!(!keep(&c, &wider, fresh));
    }

    /// The map laid out for a `width` x `height` screen, as the model does:
    /// the picture inside the pane's border, one pin "Lew Mobile" at
    /// `(39, row)`.
    fn laid_out(cell_px: (u16, u16), width: u16, height: u16, row: u16) -> MapView {
        let area = leviculum_ui::Rect::new(1, 1, width - 2, height - 3);
        let at = project(LatLon::from_degrees(48.0, 10.0));
        MapView {
            area,
            viewport: Viewport::new(
                at,
                10,
                u32::from(area.width) * u32::from(cell_px.0),
                u32::from(area.height) * u32::from(cell_px.1),
            ),
            cell_px,
            tracks: Vec::new(),
            radar: None,
            window_word: None,
            moving: false,
            pins: vec![PinCell {
                peer: [1; 16],
                col: 39,
                row,
                text: "● Lew Mobile · 1 min".to_string(),
                apart: None,
                stale: false,
                nearby: false,
                selected: false,
                own: false,
            }],
        }
    }

    fn every_backend() -> Vec<MapPainter> {
        Graphics::ORDER
            .iter()
            .map(|&g| match g {
                Graphics::Sixel => MapPainter::new(Maps::default(), g, Some(Picker::halfblocks())),
                _ => MapPainter::new(Maps::default(), g, None),
            })
            .collect()
    }

    /// The terminal shrank (a phone's keyboard came up) and the frame is
    /// drawn before the model heard of it: the view is laid out for 54 x 60,
    /// the frame is 54 x 30. Nothing is written outside it (1087: "index
    /// outside of buffer ... is (39, 49)").
    #[test]
    fn a_view_laid_out_for_a_taller_screen_draws_in_a_shorter_frame() {
        for mut painter in every_backend() {
            let graphics = painter.graphics();
            let tall = laid_out(painter.setup().cell_px, 54, 60, 49);
            let mut terminal = Terminal::new(TestBackend::new(54, 60)).expect("terminal");
            terminal
                .draw(|f| painter.render(f, &tall, Style::default()))
                .expect("tall");
            terminal.backend_mut().resize(54, 30);
            terminal
                .draw(|f| painter.render(f, &tall, Style::default()))
                .expect("short");
            let area = terminal.backend().buffer().area;
            assert_eq!((area.width, area.height), (54, 30), "{graphics:?}");
        }
    }

    /// The same shrink, heard by the model, on the painter's thread: the
    /// first frame at 54 x 30 still shows the 54 x 60 paint, and with it
    /// the labels placed for that paint, one of them on row 49.
    #[test]
    fn a_paint_for_the_old_size_does_not_place_its_labels_in_the_new_one() {
        for painter in every_backend() {
            let mut painter = painter.threaded();
            let graphics = painter.graphics();
            let cell_px = painter.setup().cell_px;
            let tall = laid_out(cell_px, 54, 60, 49);
            let mut terminal = Terminal::new(TestBackend::new(54, 60)).expect("terminal");
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                terminal
                    .draw(|f| painter.render(f, &tall, Style::default()))
                    .expect("tall");
                if painter.painted_view() == Some(tall.viewport) || Instant::now() > deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            assert_eq!(painter.painted_view(), Some(tall.viewport), "{graphics:?}");
            terminal.backend_mut().resize(54, 30);
            let short = laid_out(cell_px, 54, 30, 20);
            terminal
                .draw(|f| painter.render(f, &short, Style::default()))
                .expect("short");
            assert_eq!(
                painter.painted_view(),
                Some(tall.viewport),
                "{graphics:?}: the old paint still up"
            );
        }
    }

    /// The thread end to end: a frame asks, the next frame after the paint
    /// finished shows it, and a frame of the same view asks nothing more.
    #[test]
    fn the_threaded_painter_shows_the_view_asked_for() {
        let mut painter = MapPainter::new(Maps::default(), Graphics::Braille, None).threaded();
        let (a, b) = (ask_east(0), ask_east(5));
        let now = Instant::now();
        painter.paint(a, now);
        let deadline = Instant::now() + Duration::from_secs(10);
        while painter.painted_view() != Some(a.view) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
            painter.paint(a, Instant::now());
        }
        assert_eq!(painter.painted_view(), Some(a.view));
        assert_eq!(painter.paints(), 1);
        // B asked a moment after A went up: B, when it comes, is the view
        // wanted and is shown however soon.
        painter.paint(b, Instant::now());
        while painter.painted_view() != Some(b.view) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
            painter.paint(b, Instant::now());
        }
        assert_eq!(painter.painted_view(), Some(b.view));
        assert_eq!(painter.paints(), 2);
    }

    /// The Florence fixture as a maps directory.
    fn florence() -> Maps {
        Maps::new(Some(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../leviculum-map/fixtures"),
        ))
    }

    /// A sixel map of Florence at z15, 60 x 20 cells, no pin.
    fn florence_sixel(painter: &MapPainter, centre: World) -> MapView {
        let cell_px = painter.setup().cell_px;
        let (cols, rows) = (60u16, 20u16);
        MapView {
            tracks: Vec::new(),
            radar: None,
            window_word: None,
            moving: false,
            area: leviculum_ui::Rect::new(0, 0, cols, rows),
            viewport: Viewport::new(
                centre,
                15,
                u32::from(cols) * u32::from(cell_px.0),
                u32::from(rows) * u32::from(cell_px.1),
            ),
            cell_px,
            pins: Vec::new(),
        }
    }

    /// A mouse drag over a sixel map with names, then the pointer at rest
    /// (1118): each frame sends at most one picture, every picture sent is
    /// a new view's, never the last one again for the names that moved, and
    /// a frame at rest sends none. The names are there to be counted.
    #[test]
    fn a_drag_over_names_sends_each_picture_once() {
        let mut painter = MapPainter::new(florence(), Graphics::Sixel, Some(Picker::halfblocks()));
        let (cw, ch) = painter.setup().cell_px;
        let mut shown = florence_sixel(&painter, project(LatLon::from_degrees(43.7755, 11.2672)));
        let mut terminal = Terminal::new(TestBackend::new(60, 21)).expect("terminal");
        let mut last = ratatui::buffer::Buffer::empty(Rect::new(0, 0, 60, 21));
        let mut sent: Option<String> = None;
        let mut names = 0;
        for step in 0..16 {
            if (1..12).contains(&step) {
                shown.viewport.pan(2 * i32::from(cw), i32::from(ch));
            }
            let next = terminal
                .draw(|f| painter.render(f, &shown, Style::default()))
                .expect("draw")
                .buffer
                .clone();
            let pictures: Vec<String> = last
                .diff(&next)
                .into_iter()
                .map(|(_, _, cell)| cell.symbol())
                .filter(|s| s.contains("\u{1b}P"))
                .map(|s| s.trim_start_matches(NUDGE).to_string())
                .collect();
            assert!(
                pictures.len() <= 1,
                "step {step}: {} pictures",
                pictures.len()
            );
            if let Some(picture) = pictures.into_iter().next() {
                assert!(step < 12, "step {step}: a picture sent at rest");
                assert_ne!(sent.as_ref(), Some(&picture), "step {step}: sent again");
                sent = Some(picture);
            } else {
                assert!(step >= 12, "step {step}: the new view not sent");
            }
            names += painter.labels.iter().filter(|l| l.name).count();
            last = next;
        }
        assert!(names > 0, "no names over the drag");
    }

    /// A sixel map of Florence at rest with its names, pinned as the
    /// terminal shows it: the picture's cells `▒`, the names set over it.
    #[test]
    fn a_sixel_map_at_rest_shows_its_names() {
        let mut painter = MapPainter::new(florence(), Graphics::Sixel, Some(Picker::halfblocks()));
        let shown = florence_sixel(&painter, project(LatLon::from_degrees(43.7755, 11.2672)));
        let mut terminal = Terminal::new(TestBackend::new(60, 21)).expect("terminal");
        let mut screen = Screen::new(Rect::new(0, 0, 60, 21));
        screen.frame(&mut terminal, &mut painter, &shown);
        let frame: Vec<String> = (0..20)
            .map(|y| screen.row(y).replace('▒', " ").trim_end().to_string())
            .collect();
        let frame = frame.join("\n");
        assert_eq!(frame, PINNED_SIXEL_NAMES, "{frame:?}");
    }

    const PINNED_SIXEL_NAMES: &str =
        "\n\n\n\n\n\n\n\n                    Giardino del Borgo\n\n\n\n\n\n\n\n\n\n\n";
}
