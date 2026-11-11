//! The house key's page: the M9's settings (1532, 1561).
//!
//! The house key opens the settings, the core's one structure
//! (`loquax::ui::settings`): its sections in the order every target lists
//! them, as far as the M9 draws them (`settings::PLACES`, the M9 column),
//! each a heading over its rows. A row is a [`Row`]: its label, its value,
//! and what changes the value ([`Cycle`]); the board's own rows (the type
//! size, the about lines) and the core's rows for the section share the
//! one model, so a later row (the theme, the announce cadence) is one
//! more entry here, never a second settings concept.
//!
//! The keys, as Lew asked at the board on 2026-11-08 ("mit Pfeiltasten
//! auswählen und mit Return anwählen"): Up and Down move between the rows
//! a reader can change; Left and Right step the value, Enter on a row
//! steps it once; the stepped value stands marked on the row, and Enter
//! confirms it, Back drops it and the row keeps its value. Back with
//! nothing stepped closes the page, as the house key, the chats key, the
//! new-message key and the map key do.
//!
//! No hold (1561): the controller does not repeat a held key. Lew held
//! the house key 1.2 s on the 840cdaae bench (2026-11-08 13:0x) and the
//! page opened as on a tap, so 1532's "hold the house key to switch the
//! type" never fired on the board (the walker's `hold` injects repeats,
//! which is why window 13 saw `via=hold`). The page opens on the press,
//! with no 300 ms wait for a second one.
//!
//! The size is the reader's and lives in the client record's two bits
//! (`loquax::home::record`); a board without a home keeps it until the
//! power goes.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use leviculum_ui::input::{KeyCode, KeyEvent, KeyEventKind};
use leviculum_ui::style::{RLine, RStyle};
use loquax::ui::settings::{self, Section, Target};
use loquax::ui::strings::{StrId, Text};
use loquax::ui::UiModel;

use crate::font::Size;
use crate::keyboard::{Key, Role};
use crate::screen::{fill, muted, painted, push, Look, BAR};

/// What changes a row's value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cycle {
    /// The reader's type size, through [`Size::ALL`].
    Size,
    /// A value the M9 shows and does not change: the about lines.
    Fixed,
}

/// One row of the page: its label, its value, what changes it.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub label: Text,
    pub value: Option<Text>,
    pub cycle: Cycle,
}

impl Row {
    /// Whether the reader can move onto it and change it.
    pub fn changes(&self) -> bool {
        self.cycle != Cycle::Fixed
    }
}

/// A section of the page: the core's title over the rows.
#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    pub section: Section,
    pub title: Text,
    pub rows: Vec<Row>,
}

/// The page as the board draws it: the core's title and the sections the
/// M9 draws, each with its rows.
#[derive(Clone, Debug, PartialEq)]
pub struct Page {
    pub title: Text,
    pub parts: Vec<Part>,
}

impl Page {
    /// The rows a reader can change, in the page's order: what Up and
    /// Down move between.
    pub fn changing(&self) -> impl Iterator<Item = &Row> {
        self.parts
            .iter()
            .flat_map(|part| part.rows.iter())
            .filter(|row| row.changes())
    }
}

/// The name of a size on its row.
pub const fn size_word(size: Size) -> StrId {
    match size {
        Size::Tiny => StrId::M9TypeTiny,
        Size::Small => StrId::M9TypeSmall,
        Size::Large => StrId::M9TypeLarge,
    }
}

/// The page of `model` with the board's type in `size`: the core's
/// settings screen for the M9 (`settings::screen`), each section's own
/// rows (the core's, as `settings::rows` gives them for the M9) after the
/// board's. `version` is the image's, said on the about row.
pub fn page<B>(model: &UiModel<B>, size: Size, version: &str) -> Page {
    let screen = settings::screen(model, Target::M9, None);
    let parts = screen
        .entries
        .iter()
        .map(|entry| {
            let mut rows = board_rows(entry.section, size, version);
            rows.extend(
                settings::rows(model, Target::M9, entry.section)
                    .into_iter()
                    .map(|row| Row {
                        label: row.label,
                        value: row.value,
                        cycle: Cycle::Fixed,
                    }),
            );
            Part {
                section: entry.section,
                title: entry.title.clone(),
                rows,
            }
        })
        .collect();
    Page {
        title: screen.title,
        parts,
    }
}

/// The rows the board has of its own in `section`.
fn board_rows(section: Section, size: Size, version: &str) -> Vec<Row> {
    match section {
        // The type size is the board's: the core's model has no cells.
        Section::Appearance => Vec::from([Row {
            label: Text::key(StrId::M9Type),
            value: Some(Text::key(size_word(size))),
            cycle: Cycle::Size,
        }]),
        // The about lines 1531 owes: the image's version and the map's
        // data, credited as its corner does.
        Section::Advanced => Vec::from([
            Row {
                label: Text::key(StrId::M9Version),
                value: Some(Text::Raw(String::from(version))),
                cycle: Cycle::Fixed,
            },
            Row {
                label: Text::key(StrId::M9MapData),
                value: Some(Text::Raw(String::from("© OpenStreetMap"))),
                cycle: Cycle::Fixed,
            },
        ]),
        _ => Vec::new(),
    }
}

/// What a key did to the page and the size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Did {
    /// The screen changed: the page opened or closed, the cursor or a
    /// stepped value moved, the size changed.
    pub redraw: bool,
    /// The size the reader confirmed, to set and to keep.
    pub size: Option<Size>,
    /// How, for the board's `[TYPE]` line: `row`.
    pub via: &'static str,
}

/// The page, the size, the cursor and a stepped value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Overview {
    /// The page is the screen.
    pub open: bool,
    /// The reader's size.
    pub size: Size,
    /// The row under the cursor, among the rows a reader can change.
    pub at: usize,
    /// The value stepped on the cursor's row and not yet confirmed: its
    /// place in the row's values ([`Size::ALL`] for the size).
    pub stepped: Option<usize>,
}

impl Overview {
    /// Closed, in `size`.
    pub fn new(size: Size) -> Self {
        Self {
            open: false,
            size,
            at: 0,
            stepped: None,
        }
    }

    /// The size the record keeps, set without a key: the boot's, or a
    /// join's record read back.
    pub fn restore(&mut self, size: Size) -> bool {
        let moved = self.size != size;
        self.size = size;
        moved
    }

    /// A press of `key` on `page`, before the board's own handling. Some
    /// when the page took it, None when the board handles it as before:
    /// a key with a command (the chats, new-message and map keys) closes
    /// the page and is the board's; the house key opens the page or
    /// closes it; on the open page every other key is its.
    pub fn press(&mut self, key: Key, page: &Page) -> Option<Did> {
        if key == Key::Role(Role::Home) {
            self.open = !self.open;
            self.at = 0;
            self.stepped = None;
            return Some(Did {
                redraw: true,
                ..Did::default()
            });
        }
        if key.command().is_some() {
            self.open = false;
            self.stepped = None;
            return None;
        }
        if !self.open {
            return None;
        }
        let did = match key.event() {
            Some(event) => self.key(&event, page),
            None => Did::default(),
        };
        Some(did)
    }

    /// A key on the open page.
    fn key(&mut self, event: &KeyEvent, page: &Page) -> Did {
        if event.kind == KeyEventKind::Release {
            return Did::default();
        }
        let rows = page.changing().count();
        let redraw = Did {
            redraw: true,
            ..Did::default()
        };
        match event.code {
            KeyCode::Up | KeyCode::Down => {
                let at = match event.code {
                    KeyCode::Up => self.at.saturating_sub(1),
                    _ => (self.at + 1).min(rows.saturating_sub(1)),
                };
                let moved = at != self.at || self.stepped.is_some();
                self.at = at;
                self.stepped = None;
                Did {
                    redraw: moved,
                    ..Did::default()
                }
            }
            KeyCode::Left | KeyCode::Right => {
                let Some((now, count)) = self.values(page) else {
                    return Did::default();
                };
                let from = self.stepped.unwrap_or(now);
                let to = match event.code {
                    KeyCode::Left => from.saturating_sub(1),
                    _ => (from + 1).min(count - 1),
                };
                self.stepped = Some(to);
                redraw
            }
            KeyCode::Enter => match self.stepped.take() {
                Some(to) => self.confirm(page, to),
                None => {
                    let Some((now, count)) = self.values(page) else {
                        return Did::default();
                    };
                    self.stepped = Some((now + 1) % count);
                    redraw
                }
            },
            KeyCode::Esc => {
                if self.stepped.take().is_none() {
                    self.open = false;
                }
                redraw
            }
            _ => Did::default(),
        }
    }

    /// The cursor row's value now and how many it has; None where no row
    /// can change.
    fn values(&self, page: &Page) -> Option<(usize, usize)> {
        match page.changing().nth(self.at)?.cycle {
            Cycle::Size => Some((self.size.index(), Size::ALL.len())),
            Cycle::Fixed => None,
        }
    }

    /// The stepped value `to` confirmed on the cursor's row.
    fn confirm(&mut self, page: &Page, to: usize) -> Did {
        let row = page.changing().nth(self.at).map(|row| row.cycle);
        let size = match (row, Size::ALL.get(to)) {
            (Some(Cycle::Size), Some(&size)) if size != self.size => size,
            _ => {
                return Did {
                    redraw: true,
                    ..Did::default()
                }
            }
        };
        self.size = size;
        Did {
            redraw: true,
            size: Some(size),
            via: "row",
        }
    }

    /// The page's screen, `cols` x `rows` cells: the bar with the title
    /// and the home's word, each section's title over its rows, the
    /// cursor's row behind the selection bar with its value reversed (a
    /// stepped one between `<` and `>`), and under the rows what Enter
    /// and Back do.
    pub fn lines(&self, page: &Page, look: &Look, cols: usize, rows: usize) -> Vec<RLine> {
        let chrome = painted(look.tokens.chrome);
        let plain = RStyle::default();
        let mut bar = RLine::default();
        let title = page.title.resolve(look.lang);
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

        let selection = painted(look.tokens.selection);
        let mut changing = 0;
        for part in &page.parts {
            out.push(fill(RLine::default(), cols, plain));
            let mut head = RLine::default();
            let heading = part.title.resolve(look.lang);
            push(
                &mut head,
                &format!(" {heading}"),
                RStyle {
                    bold: true,
                    ..muted(look)
                },
            );
            out.push(fill(head, cols, plain));
            for row in &part.rows {
                let here = row.changes() && changing == self.at;
                if row.changes() {
                    changing += 1;
                }
                out.push(self.row(row, here, look, selection, cols));
            }
        }
        let hint = match (self.stepped, page.changing().next()) {
            (Some(_), _) => Some(StrId::M9SettingsApply),
            (None, Some(_)) => Some(StrId::M9SettingsChange),
            (None, None) => None,
        };
        if let Some(hint) = hint {
            out.push(fill(RLine::default(), cols, plain));
            let mut line = RLine::default();
            push(
                &mut line,
                &format!(" {}", Text::key(hint).resolve(look.lang)),
                muted(look),
            );
            out.push(fill(line, cols, plain));
        }
        while out.len() < rows {
            out.push(fill(RLine::default(), cols, plain));
        }
        out.truncate(rows);
        out
    }

    /// One row: the bar where the cursor is, the label, and the value at
    /// the right end.
    fn row(&self, row: &Row, here: bool, look: &Look, selection: RStyle, cols: usize) -> RLine {
        let plain = RStyle::default();
        let mut line = RLine::default();
        match here {
            true => push(&mut line, BAR.encode_utf8(&mut [0; 4]), selection),
            false => push(&mut line, " ", plain),
        }
        let label = row.label.resolve(look.lang);
        push(&mut line, &format!(" {label}"), plain);
        let (value, style) = match (here, self.stepped, row.cycle) {
            (true, Some(to), Cycle::Size) => {
                let size = Size::ALL.get(to).copied().unwrap_or(self.size);
                let words = Text::key(size_word(size)).resolve(look.lang);
                (
                    format!("< {words} >"),
                    RStyle {
                        bold: true,
                        reverse: true,
                        ..plain
                    },
                )
            }
            (true, _, _) => (
                row.value
                    .as_ref()
                    .map(|v| v.resolve(look.lang))
                    .unwrap_or_default(),
                RStyle {
                    bold: true,
                    reverse: true,
                    ..plain
                },
            ),
            (false, _, Cycle::Fixed) => (
                row.value
                    .as_ref()
                    .map(|v| v.resolve(look.lang))
                    .unwrap_or_default(),
                muted(look),
            ),
            (false, _, _) => (
                row.value
                    .as_ref()
                    .map(|v| v.resolve(look.lang))
                    .unwrap_or_default(),
                RStyle {
                    bold: true,
                    ..plain
                },
            ),
        };
        let width = value.chars().count();
        // The value at the right end, one cell in; a label too long for
        // both gives way to the value.
        let gap = cols.saturating_sub(line.cells.len() + width + 1).max(1);
        push(&mut line, &" ".repeat(gap), plain);
        push(&mut line, &value, style);
        fill(line, cols, plain)
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
    use crate::field::Field;
    use crate::keys::M9Key;
    use loquax::ui::strings::Lang;

    const HOUSE: Key = Key::Role(Role::Home);

    fn the_page(size: Size) -> Page {
        page(&UiModel::<Field>::new(&[]), size, "0.0.0")
    }

    fn text(overview: &Overview, size: Size) -> Vec<String> {
        let (cols, rows) = size.grid();
        overview
            .lines(
                &the_page(overview.size),
                &Look::m9(Lang::De, None),
                cols,
                rows,
            )
            .iter()
            .map(|line| line.cells.iter().map(|c| c.ch).collect())
            .collect()
    }

    fn press(o: &mut Overview, key: M9Key) -> Option<Did> {
        let page = the_page(o.size);
        o.press(Key::Named(key), &page)
    }

    /// The page is the core's settings for the M9: its title, the
    /// sections `settings::PLACES` marks drawn on the M9, in the core's
    /// order, and the board's rows in them; one row changes.
    #[test]
    fn the_page_is_the_cores_settings_for_the_m9() {
        let page = the_page(Size::Small);
        assert_eq!(page.title, Text::key(StrId::Settings));
        let sections: Vec<Section> = page.parts.iter().map(|p| p.section).collect();
        assert_eq!(sections, settings::sections(Target::M9).collect::<Vec<_>>());
        assert_eq!(sections, [Section::Appearance, Section::Advanced]);
        let changing: Vec<&Row> = page.changing().collect();
        assert_eq!(changing.len(), 1);
        assert_eq!(changing[0].label, Text::key(StrId::M9Type));
        assert_eq!(changing[0].value, Some(Text::key(StrId::M9TypeSmall)));
        assert_eq!(
            page.parts[1].rows[0].value,
            Some(Text::Raw(String::from("0.0.0")))
        );
    }

    /// The house key opens the page at once, no wait for a second press
    /// (1561: the controller repeats no held key); its rows show, Back
    /// closes it, the house key again closes it too.
    #[test]
    fn the_house_key_opens_the_page_at_once() {
        let mut o = Overview::new(Size::Small);
        let page = the_page(o.size);
        let did = o.press(HOUSE, &page).expect("the page's");
        assert!(did.redraw && o.open, "open on the press");
        let rows = text(&o, Size::Small);
        assert_eq!(rows.len(), 16);
        assert!(rows[0].starts_with(" Einstellungen"), "{rows:#?}");
        assert!(rows[2].starts_with(" Darstellung"), "{rows:#?}");
        assert!(rows[3].starts_with("▌ Schrift"), "{rows:#?}");
        assert!(rows[3].trim_end().ends_with("klein"), "{rows:#?}");
        assert!(rows.iter().any(|r| r.contains("Enter ändert")), "{rows:#?}");
        let did = press(&mut o, M9Key::Back).expect("the page's");
        assert!(did.redraw && !o.open);
        // Closed, the keys are the board's again.
        assert_eq!(press(&mut o, M9Key::Down), None);
        o.press(HOUSE, &page);
        assert!(o.open);
        o.press(HOUSE, &page);
        assert!(!o.open, "the house key closes it again");
    }

    /// Left and Right step the value on the row, Enter confirms it: the
    /// size moves, said once with `via=row`; the row then shows it.
    #[test]
    fn a_stepped_value_is_confirmed_by_enter() {
        let mut o = Overview::new(Size::Small);
        o.press(HOUSE, &the_page(o.size));
        let did = press(&mut o, M9Key::Left).expect("the page's");
        assert!(did.redraw);
        assert_eq!(did.size, None, "a step changes nothing yet");
        assert_eq!(o.stepped, Some(Size::Tiny.index()));
        assert_eq!(o.size, Size::Small);
        let rows = text(&o, Size::Small);
        assert!(rows[3].contains("< sehr klein >"), "{rows:#?}");
        assert!(
            rows.iter().any(|r| r.contains("Enter übernimmt")),
            "{rows:#?}"
        );
        // Left again stays at the smallest.
        press(&mut o, M9Key::Left);
        assert_eq!(o.stepped, Some(0));
        let did = press(&mut o, M9Key::Enter).expect("the page's");
        assert_eq!(did.size, Some(Size::Tiny));
        assert_eq!(did.via, "row");
        assert_eq!((o.size, o.stepped, o.open), (Size::Tiny, None, true));
        let rows = text(&o, Size::Tiny);
        assert_eq!(rows.len(), 20);
        assert!(rows[3].trim_end().ends_with("sehr klein"), "{rows:#?}");
        // Right twice and Enter: large, past small.
        press(&mut o, M9Key::Right);
        press(&mut o, M9Key::Right);
        press(&mut o, M9Key::Right);
        assert_eq!(o.stepped, Some(2), "the largest stays");
        let did = press(&mut o, M9Key::Enter).expect("the page's");
        assert_eq!(did.size, Some(Size::Large));
    }

    /// Enter on a row with nothing stepped steps it once, round the
    /// values; Enter again confirms.
    #[test]
    fn enter_cycles_the_value_and_enter_confirms() {
        let mut o = Overview::new(Size::Large);
        o.press(HOUSE, &the_page(o.size));
        let did = press(&mut o, M9Key::Enter).expect("the page's");
        assert_eq!(did.size, None);
        assert_eq!(o.stepped, Some(Size::Tiny.index()), "round the end");
        let did = press(&mut o, M9Key::Enter).expect("the page's");
        assert_eq!(did.size, Some(Size::Tiny));
        // A step back onto the value in force confirms nothing new.
        press(&mut o, M9Key::Right);
        press(&mut o, M9Key::Left);
        let did = press(&mut o, M9Key::Enter).expect("the page's");
        assert_eq!(did.size, None);
        assert!(did.redraw);
    }

    /// Back drops a stepped value: the size stays, the page stays open;
    /// Back again closes it. Up and Down drop it too, and stay on the one
    /// row there is.
    #[test]
    fn back_returns_without_a_change() {
        let mut o = Overview::new(Size::Small);
        o.press(HOUSE, &the_page(o.size));
        press(&mut o, M9Key::Right);
        assert_eq!(o.stepped, Some(Size::Large.index()));
        let did = press(&mut o, M9Key::Back).expect("the page's");
        assert_eq!(did.size, None);
        assert!(did.redraw && o.open);
        assert_eq!((o.size, o.stepped), (Size::Small, None));
        press(&mut o, M9Key::Right);
        let did = press(&mut o, M9Key::Down).expect("the page's");
        assert!(did.redraw, "the step went");
        assert_eq!((o.at, o.stepped, o.size), (0, None, Size::Small));
        assert_eq!(press(&mut o, M9Key::Up), Some(Did::default()));
        // A key with no verb on the page is the page's, and does nothing.
        assert_eq!(press(&mut o, M9Key::Char('x')), Some(Did::default()));
        press(&mut o, M9Key::Back);
        assert!(!o.open);
    }

    /// Up and Down move between the rows that change, skipping the fixed
    /// ones, on a page of two such rows (the control: a page the board
    /// does not draw today, the model the next row will use).
    #[test]
    fn up_and_down_move_between_the_rows_that_change() {
        let mut page = the_page(Size::Small);
        page.parts[1].rows.push(Row {
            label: Text::Raw(String::from("Zweite")),
            value: Some(Text::key(StrId::M9TypeLarge)),
            cycle: Cycle::Size,
        });
        let mut o = Overview::new(Size::Small);
        o.press(HOUSE, &page);
        let down = Key::Named(M9Key::Down);
        assert!(o.press(down, &page).expect("the page's").redraw);
        assert_eq!(o.at, 1);
        assert!(
            !o.press(down, &page).expect("the page's").redraw,
            "the last"
        );
        assert_eq!(o.at, 1);
        let (cols, rows) = Size::Small.grid();
        let lines: Vec<String> = o
            .lines(&page, &Look::m9(Lang::De, None), cols, rows)
            .iter()
            .map(|line| line.cells.iter().map(|c| c.ch).collect())
            .collect();
        let marked: Vec<&String> = lines.iter().filter(|l| l.starts_with('▌')).collect();
        assert_eq!(marked.len(), 1);
        assert!(marked[0].contains("Zweite"), "{lines:#?}");
        o.press(Key::Named(M9Key::Up), &page);
        assert_eq!(o.at, 0);
    }

    /// A command key leaves the page to the board and drops a step.
    #[test]
    fn a_command_leaves() {
        let mut o = Overview::new(Size::Large);
        let page = the_page(o.size);
        o.press(HOUSE, &page);
        o.press(Key::Named(M9Key::Right), &page);
        assert_eq!(o.press(Key::Role(Role::Map), &page), None);
        assert!(!o.open);
        assert_eq!((o.size, o.stepped), (Size::Large, None));
        assert_eq!(text(&o, Size::Large)[0].trim(), "Einstellungen");
    }

    /// Every size's page fits its grid: no row cut, the hint under them.
    #[test]
    fn the_page_fits_every_size() {
        for size in Size::ALL {
            let mut o = Overview::new(size);
            o.press(HOUSE, &the_page(size));
            press(&mut o, M9Key::Left);
            let rows = text(&o, size);
            let (cols, count) = size.grid();
            assert_eq!(rows.len(), count);
            assert!(rows.iter().all(|r| r.chars().count() == cols));
            assert!(
                rows.iter().any(|r| r.contains("Enter übernimmt")),
                "{size:?} {rows:#?}"
            );
            assert!(
                rows.iter().any(|r| r.contains("© OpenStreetMap")),
                "{size:?} {rows:#?}"
            );
        }
    }
}
