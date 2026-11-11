//! The house key's page and the type switch (1532).
//!
//! The house key opens the overview, a screen of the M9's own: its title,
//! the home's word at the bar's right end, and the row that switches the
//! type, "Schrift: klein | groß", the size in use marked. Enter or the
//! ring's left or right on the row switches; Back, the chats key and the
//! map key leave. Before 1532 the house key brought the chats list, as
//! the chats key does; nothing was bound to holding it.
//!
//! Holding the house key a second switches the size from wherever the
//! keys are, with a short word in the top bar ("Schrift: groß"). The
//! controller reports presses, never a release (`keyboard`'s module
//! docs), so a hold is seen only as the presses a held key repeats: house
//! presses no more than [`GAP_MS`] apart for [`HOLD_MS`] are a hold.
//! Whether the controller repeats a held key is not measured yet
//! (`kbraw::HOLD_REPEAT_MS`); if it does not, a hold is one press and
//! opens the overview, whose row switches the size. So that a hold never
//! first leaves the screen it started on, a house press opens the page
//! only once [`GAP_MS`] passed without another, or when another key
//! comes.
//!
//! The size is the reader's and lives in the client record's flag
//! (`loquax::home::record`); a board without a home keeps it until the
//! power goes.

use alloc::format;
use alloc::vec::Vec;

use leviculum_ui::input::{KeyCode, KeyEvent, KeyEventKind};
use leviculum_ui::style::{RLine, RStyle};
use loquax::ui::strings::{StrId, Text};

use crate::font::Size;
use crate::keyboard::{Key, Role};
use crate::screen::{fill, muted, painted, push, Look, BAR};

/// How long the house key is held before the size switches.
pub const HOLD_MS: u32 = 1000;

/// The longest gap between two presses of one hold: the controller's
/// repeat, if it has one, is near 200 ms (`kbraw::HOLD_REPEAT_MS`). A
/// house press opens the overview once this passed without another.
pub const GAP_MS: u32 = 300;

/// How long the word of a switch stands in the top bar.
pub const NOTE_MS: u32 = 1500;

/// What a key or the clock did to the overview and the size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Did {
    /// The screen changed: the page opened or closed, the size moved, the
    /// word came or went.
    pub redraw: bool,
    /// The size the reader switched to, to set and to keep.
    pub size: Option<Size>,
    /// How: `hold` or `row`, for the board's log line.
    pub via: &'static str,
}

/// One run of house presses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Run {
    first: u32,
    last: u32,
    /// The run switched the size already: it opens nothing.
    switched: bool,
}

/// The overview, the size and the house key's presses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Overview {
    /// The overview is the screen.
    pub open: bool,
    /// The reader's size.
    pub size: Size,
    run: Option<Run>,
    /// The switch's word and until when it stands.
    note: Option<(StrId, u32)>,
}

impl Overview {
    /// Closed, in `size`.
    pub fn new(size: Size) -> Self {
        Self {
            open: false,
            size,
            run: None,
            note: None,
        }
    }

    /// The size the record keeps, set without a word: the boot's, or a
    /// join's record read back.
    pub fn restore(&mut self, size: Size) -> bool {
        let moved = self.size != size;
        self.size = size;
        moved
    }

    /// A press of `key` at `now`, before the board's own handling. Some
    /// when the overview took it, None when the board handles it as
    /// before: a key with a command (the chats, new-message and map keys)
    /// closes the overview and is the board's; any key settles a house
    /// press still waiting; on the open overview every other key is its.
    pub fn press(&mut self, key: Key, now: u32) -> Option<Did> {
        if key == Key::Role(Role::Home) {
            return Some(self.house(now));
        }
        if key.command().is_some() {
            // A tap still waiting was the overview's way in; the command
            // goes past it. The board redraws for the command anyway.
            self.run = None;
            self.open = false;
            return None;
        }
        let mut did = self.settle();
        if !self.open {
            return did.redraw.then_some(did);
        }
        if let Some(event) = key.event() {
            did = self.key(&event, did);
        }
        Some(did)
    }

    /// The clock: a house press with no other after [`GAP_MS`] opens the
    /// overview; the switch's word goes after [`NOTE_MS`].
    pub fn tick(&mut self, now: u32) -> Did {
        let mut did = Did::default();
        if let Some(run) = self.run {
            if now.wrapping_sub(run.last) > GAP_MS {
                did = self.settle();
            }
        }
        if let Some((_, until)) = self.note {
            if now.wrapping_sub(until) as i32 >= 0 {
                self.note = None;
                did.redraw = true;
            }
        }
        did
    }

    /// Whether a house press waits for its gap: the board's loop keeps
    /// turning until it settles.
    pub fn waiting(&self) -> bool {
        self.run.is_some_and(|run| !run.switched) || self.note.is_some()
    }

    /// The switch's word for the top bar, while it stands.
    pub fn note(&self) -> Option<StrId> {
        self.note.map(|(id, _)| id)
    }

    /// A house press: the start of a run, or one more of a hold, which
    /// switches the size once it is [`HOLD_MS`] long.
    fn house(&mut self, now: u32) -> Did {
        match self.run.as_mut() {
            Some(run) if now.wrapping_sub(run.last) <= GAP_MS => {
                run.last = now;
                if !run.switched && now.wrapping_sub(run.first) >= HOLD_MS {
                    run.switched = true;
                    return self.switch(now, "hold");
                }
                Did::default()
            }
            _ => {
                let did = self.settle();
                self.run = Some(Run {
                    first: now,
                    last: now,
                    switched: false,
                });
                did
            }
        }
    }

    /// The run ends: a run that switched nothing was a tap and opens the
    /// overview.
    fn settle(&mut self) -> Did {
        match self.run.take() {
            Some(run) if !run.switched && !self.open => {
                self.open = true;
                Did {
                    redraw: true,
                    ..Did::default()
                }
            }
            _ => Did::default(),
        }
    }

    /// A key on the open overview: its one row switches, Back leaves.
    fn key(&mut self, event: &KeyEvent, did: Did) -> Did {
        if event.kind == KeyEventKind::Release {
            return did;
        }
        match event.code {
            KeyCode::Enter | KeyCode::Left | KeyCode::Right => {
                let mut switched = self.switch(0, "row");
                // The row shows the size; no word over the page.
                self.note = None;
                switched.redraw = true;
                switched
            }
            KeyCode::Esc => {
                self.open = false;
                Did {
                    redraw: true,
                    ..did
                }
            }
            _ => did,
        }
    }

    fn switch(&mut self, now: u32, via: &'static str) -> Did {
        self.size = self.size.toggled();
        let word = match self.size {
            Size::Small => StrId::M9TypeNowSmall,
            Size::Large => StrId::M9TypeNowLarge,
        };
        self.note = Some((word, now.wrapping_add(NOTE_MS)));
        Did {
            redraw: true,
            size: Some(self.size),
            via,
        }
    }

    /// The overview's screen, `cols` x `rows` cells: the bar with the
    /// title and the home's word, a blank row, the type's row behind the
    /// selection bar, the size in use bold and reversed.
    pub fn lines(&self, look: &Look, cols: usize, rows: usize) -> Vec<RLine> {
        let chrome = painted(look.tokens.chrome);
        let mut bar = RLine::default();
        let title = Text::key(StrId::M9Overview).resolve(look.lang);
        push(
            &mut bar,
            &format!(" {title}"),
            RStyle {
                bold: true,
                ..chrome
            },
        );
        // The list's bar's word (1564): one short word, whole or none.
        let word = crate::screen::bar_word(look).unwrap_or_default();
        let room = cols.saturating_sub(bar.cells.len() + 2);
        if !word.is_empty() && word.chars().count() <= room {
            let gap = cols.saturating_sub(bar.cells.len() + word.chars().count() + 1);
            push(&mut bar, &" ".repeat(gap), chrome);
            push(&mut bar, &word, chrome);
        }
        let mut out = Vec::with_capacity(rows);
        out.push(fill(bar, cols, chrome));
        out.push(fill(RLine::default(), cols, RStyle::default()));

        let selection = painted(look.tokens.selection);
        let mut row = RLine::default();
        push(&mut row, BAR.encode_utf8(&mut [0; 4]), selection);
        let label = Text::key(StrId::M9Type).resolve(look.lang);
        push(&mut row, &format!(" {label}: "), RStyle::default());
        for (i, size) in Size::ALL.into_iter().enumerate() {
            if i > 0 {
                push(&mut row, " | ", muted(look));
            }
            let id = match size {
                Size::Small => StrId::M9TypeSmall,
                Size::Large => StrId::M9TypeLarge,
            };
            let words = Text::key(id).resolve(look.lang);
            let style = match size == self.size {
                true => RStyle {
                    bold: true,
                    reverse: true,
                    ..RStyle::default()
                },
                false => muted(look),
            };
            push(&mut row, &words, style);
        }
        out.push(fill(row, cols, RStyle::default()));
        while out.len() < rows {
            out.push(fill(RLine::default(), cols, RStyle::default()));
        }
        out.truncate(rows);
        out
    }
}

/// Set `size` on what draws (1532): the renderer's cells and the
/// model's window, which the scene is built for. What the model asks
/// back is the caller's to route (`home::route`): the fold asks the
/// selected conversation's page from the ring when none of it is loaded,
/// once (1546; dropped here before, the boot's cursor chat never loaded
/// and opened with no read mark).
#[must_use = "the model's asks, a page read among them, go to home::route"]
pub fn set_size<D>(
    size: Size,
    model: &mut loquax::ui::UiModel<crate::field::Field>,
    renderer: &mut crate::render::Renderer<'_, D>,
) -> alloc::vec::Vec<loquax::ui::UiEffect>
where
    D: embedded_graphics::draw_target::DrawTarget<Color = embedded_graphics::pixelcolor::Rgb565>
        + 'static,
{
    renderer.set_size(size);
    loquax::ui::update(
        model,
        loquax::ui::UiEvent::Viewing(crate::screen::viewing(size)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::M9Key;
    use alloc::string::String;
    use loquax::ui::strings::Lang;

    fn text(overview: &Overview, size: Size) -> Vec<String> {
        let (cols, rows) = size.grid();
        overview
            .lines(&Look::m9(Lang::De, None), cols, rows)
            .iter()
            .map(|line| line.cells.iter().map(|c| c.ch).collect())
            .collect()
    }

    const HOUSE: Key = Key::Role(Role::Home);

    /// A tap opens the overview once its gap passed, not before; the
    /// page's row says the size, Enter switches it, Back leaves.
    #[test]
    fn a_tap_opens_the_overview_and_its_row_switches() {
        let mut o = Overview::new(Size::Small);
        assert_eq!(o.press(HOUSE, 1000), Some(Did::default()));
        assert!(!o.open, "not before the gap");
        assert!(o.waiting());
        assert_eq!(o.tick(1000 + GAP_MS), Did::default(), "the gap's end");
        assert!(o.tick(1001 + GAP_MS).redraw);
        assert!(o.open);
        let rows = text(&o, Size::Small);
        assert_eq!(rows.len(), 16);
        assert!(rows[0].starts_with(" Übersicht"), "{rows:#?}");
        assert!(rows[2].contains("Schrift: klein | groß"), "{rows:#?}");
        let did = o.press(Key::Named(M9Key::Enter), 2000).expect("the page's");
        assert_eq!(did.size, Some(Size::Large));
        assert_eq!(did.via, "row");
        assert_eq!(o.size, Size::Large);
        assert_eq!(o.note(), None, "the row shows it");
        // A key with no verb on the page is the page's, and does nothing.
        assert_eq!(
            o.press(Key::Named(M9Key::Char('x')), 2100),
            Some(Did::default())
        );
        let did = o.press(Key::Named(M9Key::Back), 2200).expect("the page's");
        assert!(did.redraw && !o.open);
        // Closed, the keys are the board's again.
        assert_eq!(o.press(Key::Named(M9Key::Down), 2300), None);
    }

    /// A hold, the presses a held key repeats, switches the size after a
    /// second without opening the page; its word stands, then goes. The
    /// control: the same presses further apart than the gap are taps,
    /// and switch nothing.
    #[test]
    fn a_held_house_key_switches_the_size_and_opens_nothing() {
        let mut o = Overview::new(Size::Small);
        let mut switched = Vec::new();
        for t in (0..=1400).step_by(200) {
            if let Some(Did {
                size: Some(size), ..
            }) = o.press(HOUSE, 5000 + t)
            {
                switched.push((t, size));
            }
        }
        assert_eq!(switched, [(1000, Size::Large)], "once, at a second");
        assert_eq!(o.note(), Some(StrId::M9TypeNowLarge));
        let did = o.tick(6400 + GAP_MS + 1);
        assert!(!o.open && !did.redraw, "a hold opens nothing");
        assert!(o.tick(6000 + NOTE_MS).redraw, "the word goes");
        assert_eq!(o.note(), None);
        assert!(!o.waiting());

        let mut taps = Overview::new(Size::Small);
        for t in (0..=1400).step_by(400) {
            let did = taps.press(HOUSE, 9000 + t);
            assert_eq!(did.and_then(|d| d.size), None);
        }
        assert_eq!(taps.size, Size::Small);
    }

    /// A command key leaves the overview to the board; another key while
    /// a tap waits opens the page first and is then the page's.
    #[test]
    fn a_command_leaves_and_a_quick_key_lands_on_the_page() {
        let mut o = Overview::new(Size::Large);
        o.press(HOUSE, 0);
        let did = o.press(Key::Named(M9Key::Right), 100).expect("the page's");
        assert!(o.open && did.redraw);
        assert_eq!(did.size, Some(Size::Small));
        assert_eq!(o.press(Key::Role(Role::Map), 200), None);
        assert!(!o.open);
        assert_eq!(text(&o, Size::Small)[0].trim(), "Übersicht");
    }
}
