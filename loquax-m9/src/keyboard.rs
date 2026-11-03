//! The M9's keyboard: scan frames to the key events the key table reads
//! (`handheld-messenger.md`, "The input model"; slice three, 1359).
//!
//! What the board's pages say, and only that: 37 keys plus navigation and
//! function keys behind a controller on its own I2C bus, which ACKs at
//! 0x6D on our V1.1 unit (`thinknode-m9.md`, "Two hardware revisions"),
//! reading a 5 x 10 matrix (the wiki, "Keyboard"). Its INT line is a 15 ms
//! pulse that does not come for every key, so the firmware polls while the
//! panel is lit (`thinknode-m9.md`, "The keypad"). The controller's
//! register map, which position is which key, and whether it repeats or
//! knows a layer itself are unread: the board answers them in slice four,
//! and nothing here takes them from a foreign header.
//!
//! So the driver starts at a [`Scan`]: which of the 50 matrix positions
//! read pressed in one poll. Everything after it is the board's own and
//! testable on the host:
//!
//! - **Debounce.** A position changes state only after [`DEBOUNCE_POLLS`]
//!   polls in a row read it the same way, each position on its own, so a
//!   second key pressed while the first is still down is seen (rollover).
//! - **Layers.** A [`Cap`] carries a key's plain, shifted and symbol
//!   character. Shift and Sym held with a key apply to it; tapped alone
//!   (pressed and released with no key between) they apply to the next key
//!   only, which is how a thumb keyboard is typed with one thumb.
//! - **Repeat.** A character, Backspace or an arrow held past
//!   [`REPEAT_DELAY_MS`] repeats every [`REPEAT_EVERY_MS`]; Enter and Back
//!   never do, a held Enter sends once.
//!
//! Which cell of the matrix is which cap is a [`Layout`]. The board's is
//! read in slice four with [`parse_recording`]'s line format: the firmware
//! prints one line per poll over the debug CDC, a person presses each key
//! once, and the lines become the table and the first recorded fixture.

use leviculum_ui::input::{KeyEvent, KeyEventKind};

use crate::keys::{event, M9Key};

/// The matrix's rows, from the wiki's 5 x 10 (`thinknode-m9.md`, "Keyboard").
pub const ROWS: usize = 5;
/// The matrix's columns.
pub const COLUMNS: usize = 10;
/// The matrix's positions.
pub const POSITIONS: usize = ROWS * COLUMNS;
/// The bytes of one scan frame: 50 bits, the last six always clear.
pub const FRAME_BYTES: usize = 7;

/// How often the firmware polls while the panel is lit. Below the 15 ms
/// INT pulse the community port measured, so a press that raises INT is
/// read before INT falls; the controller's own scan rate is unread, and
/// slice four measures whether the polls see every press at this rate.
pub const POLL_MS: u32 = 10;
/// How many polls in a row must read a position the same way before it
/// changes state: 20 ms at [`POLL_MS`], under the 30 to 50 ms a person
/// needs between two presses of one key.
pub const DEBOUNCE_POLLS: u8 = 2;
/// How long a key is held before it repeats.
pub const REPEAT_DELAY_MS: u32 = 500;
/// How often a held key repeats after that.
pub const REPEAT_EVERY_MS: u32 = 80;

/// Which positions read pressed in one poll: bit `row * COLUMNS + column`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Scan(u64);

impl Scan {
    /// No key down.
    pub const NONE: Self = Self(0);

    /// The scan with these `(row, column)` positions down; one outside the
    /// matrix is dropped.
    pub fn of(down: &[(usize, usize)]) -> Self {
        Self(
            down.iter()
                .filter(|(row, column)| *row < ROWS && *column < COLUMNS)
                .fold(0, |bits, (row, column)| {
                    bits | 1 << (row * COLUMNS + column)
                }),
        )
    }

    /// A frame's bytes as a scan: bit `i` is bit `i % 8` of byte `i / 8`,
    /// least significant first. The six bits past the matrix are dropped.
    pub fn from_frame(frame: &[u8; FRAME_BYTES]) -> Self {
        let bits = frame
            .iter()
            .enumerate()
            .fold(0u64, |bits, (i, byte)| bits | u64::from(*byte) << (8 * i));
        Self(bits & ((1 << POSITIONS) - 1))
    }

    /// The scan as a frame, [`Scan::from_frame`] read back.
    pub fn frame(self) -> [u8; FRAME_BYTES] {
        let mut frame = [0u8; FRAME_BYTES];
        for (i, byte) in frame.iter_mut().enumerate() {
            *byte = (self.0 >> (8 * i)) as u8;
        }
        frame
    }

    /// Whether `position` reads pressed.
    pub fn is_down(self, position: usize) -> bool {
        position < POSITIONS && self.0 & 1 << position != 0
    }
}

/// What one matrix position is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cap {
    /// No key sits there.
    None,
    /// A character key: plain, with Shift, with Sym.
    Char {
        plain: char,
        shifted: char,
        sym: char,
    },
    /// A key that is not a character: the arrows, Enter, Back, Backspace.
    Key(M9Key),
    /// The shift layer.
    Shift,
    /// The symbol layer, where the digits and punctuation live.
    Sym,
}

impl Cap {
    /// A letter: its capital with Shift, `sym` on the symbol layer.
    pub const fn letter(plain: char, sym: char) -> Self {
        Self::Char {
            plain,
            shifted: plain.to_ascii_uppercase(),
            sym,
        }
    }

    /// Whether a held key of this cap repeats.
    fn repeats(self) -> bool {
        match self {
            Self::Char { .. } => true,
            Self::Key(key) => matches!(
                key,
                M9Key::Up | M9Key::Down | M9Key::Left | M9Key::Right | M9Key::Backspace
            ),
            Self::None | Self::Shift | Self::Sym => false,
        }
    }
}

/// Which cap sits at which position, row by row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout(pub [[Cap; COLUMNS]; ROWS]);

impl Layout {
    fn cap(&self, position: usize) -> Cap {
        self.0[position / COLUMNS][position % COLUMNS]
    }
}

/// A layer a tap set for the next key, or held now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Layers {
    shift: bool,
    sym: bool,
}

/// The driver: one per keyboard, fed one [`Scan`] per poll.
#[derive(Clone, Debug)]
pub struct Keyboard<'a> {
    layout: &'a Layout,
    debounce: u8,
    /// The debounced state.
    stable: Scan,
    /// Per position, how many polls in a row read it unlike `stable`.
    against: [u8; POSITIONS],
    /// The layers a tap left for the next key.
    latched: Layers,
    /// A layer key is down and no other key has gone down since it did:
    /// its release is a tap.
    tapping: bool,
    /// The repeating key: its position, the event it repeats, and when
    /// the next repeat is due.
    held: Option<(usize, KeyEvent, u32)>,
}

impl<'a> Keyboard<'a> {
    /// A driver over `layout`, debouncing over [`DEBOUNCE_POLLS`].
    pub fn new(layout: &'a Layout) -> Self {
        Self::with_debounce(layout, DEBOUNCE_POLLS)
    }

    /// A driver that changes a position's state after `polls` alike polls
    /// (at least one).
    pub fn with_debounce(layout: &'a Layout, polls: u8) -> Self {
        Self {
            layout,
            debounce: polls.max(1),
            stable: Scan::NONE,
            against: [0; POSITIONS],
            latched: Layers::default(),
            tapping: false,
            held: None,
        }
    }

    /// One poll at `now_ms` (any monotonic millisecond clock, wrapping)
    /// that read `scan`: each key event it makes goes to `emit`, in the
    /// order the positions settled.
    pub fn poll(&mut self, now_ms: u32, scan: Scan, emit: &mut dyn FnMut(KeyEvent)) {
        for position in 0..POSITIONS {
            if scan.is_down(position) == self.stable.is_down(position) {
                self.against[position] = 0;
                continue;
            }
            self.against[position] = self.against[position].saturating_add(1);
            if self.against[position] < self.debounce {
                continue;
            }
            self.against[position] = 0;
            self.stable.0 ^= 1 << position;
            match self.stable.is_down(position) {
                true => self.pressed(position, now_ms, emit),
                false => self.released(position),
            }
        }
        if let Some((position, key, due)) = self.held {
            if now_ms.wrapping_sub(due) < u32::MAX / 2 {
                emit(KeyEvent {
                    kind: KeyEventKind::Repeat,
                    ..key
                });
                self.held = Some((position, key, due.wrapping_add(REPEAT_EVERY_MS)));
            }
        }
    }

    fn pressed(&mut self, position: usize, now_ms: u32, emit: &mut dyn FnMut(KeyEvent)) {
        let cap = self.layout.cap(position);
        let key = match cap {
            Cap::None => return,
            Cap::Shift | Cap::Sym => {
                self.tapping = true;
                return;
            }
            Cap::Key(key) => key,
            Cap::Char {
                plain,
                shifted,
                sym,
            } => {
                let layers = self.layers();
                M9Key::Char(match (layers.sym, layers.shift) {
                    (true, _) => sym,
                    (false, true) => shifted,
                    (false, false) => plain,
                })
            }
        };
        self.tapping = false;
        self.latched = Layers::default();
        let key = event(key);
        emit(key);
        self.held = cap
            .repeats()
            .then(|| (position, key, now_ms.wrapping_add(REPEAT_DELAY_MS)));
    }

    fn released(&mut self, position: usize) {
        if self.held.is_some_and(|(held, _, _)| held == position) {
            self.held = None;
        }
        let cap = self.layout.cap(position);
        if !matches!(cap, Cap::Shift | Cap::Sym) || !self.tapping {
            return;
        }
        self.tapping = false;
        match cap {
            Cap::Shift => self.latched.shift = !self.latched.shift,
            _ => self.latched.sym = !self.latched.sym,
        }
    }

    /// The layers in force: a layer key held, or one a tap latched.
    fn layers(&self) -> Layers {
        let mut layers = self.latched;
        for position in 0..POSITIONS {
            if !self.stable.is_down(position) {
                continue;
            }
            match self.layout.cap(position) {
                Cap::Shift => layers.shift = true,
                Cap::Sym => layers.sym = true,
                _ => {}
            }
        }
        layers
    }
}

/// Why a recording line did not read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordingError {
    /// The line at this number (from one) is not `KB <ms> <14 hex digits>`.
    Line(usize),
}

/// The polls of a recording: one line per poll, `KB <ms> <frame>`, the
/// frame [`FRAME_BYTES`] bytes as 14 hex digits, byte 0 first. Blank lines
/// and lines starting with `#` are comments; any other line not starting
/// with `KB ` is the firmware's other log and is skipped, so a debug CDC
/// capture reads as it was taken.
pub fn parse_recording(
    text: &str,
) -> impl Iterator<Item = Result<(u32, Scan), RecordingError>> + '_ {
    text.lines().enumerate().filter_map(|(number, line)| {
        let line = line.trim();
        let rest = line.strip_prefix("KB ")?;
        Some(parse_line(rest).ok_or(RecordingError::Line(number + 1)))
    })
}

fn parse_line(rest: &str) -> Option<(u32, Scan)> {
    let mut words = rest.split_whitespace();
    let ms = words.next()?.parse().ok()?;
    let hex = words.next()?;
    if words.next().is_some() || hex.len() != 2 * FRAME_BYTES {
        return None;
    }
    let mut frame = [0u8; FRAME_BYTES];
    for (i, byte) in frame.iter_mut().enumerate() {
        *byte = u8::from_str_radix(hex.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some((ms, Scan::from_frame(&frame)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;
    use leviculum_ui::input::KeyCode;

    /// A test layout, not the board's: q to p on row 0, a to l and
    /// Backspace on row 1, Shift, z to m, Sym on row 2, the arrows,
    /// Enter, Back and the space on row 3.
    fn layout() -> Layout {
        let mut cells = [[Cap::None; COLUMNS]; ROWS];
        for (column, (plain, sym)) in "qwertyuiop".chars().zip("1234567890".chars()).enumerate() {
            cells[0][column] = Cap::letter(plain, sym);
        }
        for (column, (plain, sym)) in "asdfghjkl".chars().zip("@#&*-+=()".chars()).enumerate() {
            cells[1][column] = Cap::letter(plain, sym);
        }
        cells[1][9] = Cap::Key(M9Key::Backspace);
        cells[2][0] = Cap::Shift;
        for (column, (plain, sym)) in "zxcvbnm".chars().zip("!?,.;:'".chars()).enumerate() {
            cells[2][column + 1] = Cap::letter(plain, sym);
        }
        cells[2][9] = Cap::Sym;
        let keys = [M9Key::Up, M9Key::Down, M9Key::Left, M9Key::Right];
        for (column, key) in keys.into_iter().enumerate() {
            cells[3][column] = Cap::Key(key);
        }
        cells[3][4] = Cap::Key(M9Key::Enter);
        cells[3][5] = Cap::Key(M9Key::Back);
        cells[3][6] = Cap::Char {
            plain: ' ',
            shifted: ' ',
            sym: ' ',
        };
        Layout(cells)
    }

    const H: (usize, usize) = (1, 5);
    const I: (usize, usize) = (0, 7);
    const SHIFT: (usize, usize) = (2, 0);
    const SYM: (usize, usize) = (2, 9);
    const ENTER: (usize, usize) = (3, 4);
    const BACKSPACE: (usize, usize) = (1, 9);

    /// Feed `polls` (each scan held for one poll, [`POLL_MS`] apart) and
    /// collect the events.
    fn run(keyboard: &mut Keyboard<'_>, polls: &[Scan]) -> Vec<KeyEvent> {
        let mut events = Vec::new();
        for (i, scan) in polls.iter().enumerate() {
            keyboard.poll(i as u32 * POLL_MS, *scan, &mut |e| events.push(e));
        }
        events
    }

    fn chars(events: &[KeyEvent]) -> alloc::string::String {
        events
            .iter()
            .filter_map(|e| match e.code {
                KeyCode::Char(c) => Some(c),
                _ => None,
            })
            .collect()
    }

    fn down(keys: &[(usize, usize)]) -> Scan {
        Scan::of(keys)
    }

    /// A key down for two polls is one press; a one-poll bounce is none.
    /// The positive control: with a debounce of one, the bounce is a press.
    #[test]
    fn a_press_settles_and_a_bounce_does_not() {
        let layout = layout();
        let press = [down(&[H]), down(&[H]), Scan::NONE, Scan::NONE];
        assert_eq!(chars(&run(&mut Keyboard::new(&layout), &press)), "h");
        let bounce = [down(&[H]), Scan::NONE, down(&[H]), Scan::NONE, Scan::NONE];
        assert_eq!(chars(&run(&mut Keyboard::new(&layout), &bounce)), "");
        let mut eager = Keyboard::with_debounce(&layout, 1);
        assert_eq!(chars(&run(&mut eager, &bounce)), "hh");
    }

    /// A second key down while the first is still held is its own press.
    #[test]
    fn rollover_sees_the_second_key() {
        let layout = layout();
        let polls = [
            down(&[H]),
            down(&[H]),
            down(&[H, I]),
            down(&[H, I]),
            down(&[I]),
            down(&[I]),
            Scan::NONE,
            Scan::NONE,
        ];
        assert_eq!(chars(&run(&mut Keyboard::new(&layout), &polls)), "hi");
    }

    /// Shift held with a key shifts it; Shift tapped alone shifts the next
    /// key only; Sym tapped gives the symbol layer once.
    #[test]
    fn shift_held_or_tapped_and_sym_tapped() {
        let layout = layout();
        let held = [
            down(&[SHIFT]),
            down(&[SHIFT]),
            down(&[SHIFT, H]),
            down(&[SHIFT, H]),
            Scan::NONE,
            Scan::NONE,
            down(&[H]),
            down(&[H]),
            Scan::NONE,
            Scan::NONE,
        ];
        assert_eq!(chars(&run(&mut Keyboard::new(&layout), &held)), "Hh");
        let tapped = [
            down(&[SHIFT]),
            down(&[SHIFT]),
            Scan::NONE,
            Scan::NONE,
            down(&[H]),
            down(&[H]),
            Scan::NONE,
            Scan::NONE,
            down(&[I]),
            down(&[I]),
            Scan::NONE,
            Scan::NONE,
            down(&[SYM]),
            down(&[SYM]),
            Scan::NONE,
            Scan::NONE,
            down(&[I]),
            down(&[I]),
            Scan::NONE,
            Scan::NONE,
            down(&[I]),
            down(&[I]),
            Scan::NONE,
            Scan::NONE,
        ];
        assert_eq!(chars(&run(&mut Keyboard::new(&layout), &tapped)), "Hi8i");
    }

    /// Shift held across a key is no tap: its release latches nothing.
    #[test]
    fn a_shift_used_as_a_chord_latches_nothing() {
        let layout = layout();
        let polls = [
            down(&[SHIFT]),
            down(&[SHIFT]),
            down(&[SHIFT, H]),
            down(&[SHIFT, H]),
            down(&[SHIFT]),
            down(&[SHIFT]),
            Scan::NONE,
            Scan::NONE,
            down(&[I]),
            down(&[I]),
            Scan::NONE,
            Scan::NONE,
        ];
        assert_eq!(chars(&run(&mut Keyboard::new(&layout), &polls)), "Hi");
    }

    /// A held letter repeats after the delay at the interval; a held
    /// Enter sends once.
    #[test]
    fn a_held_letter_repeats_and_enter_does_not() {
        let layout = layout();
        let mut keyboard = Keyboard::new(&layout);
        let mut events = Vec::new();
        // 1000 ms held: down at 10 ms, repeats due at 510, 590, ..., 990.
        for i in 0..=100u32 {
            keyboard.poll(i * POLL_MS, down(&[BACKSPACE]), &mut |e| events.push(e));
        }
        let presses = events
            .iter()
            .filter(|e| e.kind == KeyEventKind::Press)
            .count();
        let repeats = events
            .iter()
            .filter(|e| e.kind == KeyEventKind::Repeat)
            .count();
        assert_eq!((presses, repeats), (1, 7));
        assert!(events.iter().all(|e| e.code == KeyCode::Backspace));

        let mut keyboard = Keyboard::new(&layout);
        let mut events = Vec::new();
        for i in 0..=100u32 {
            keyboard.poll(i * POLL_MS, down(&[ENTER]), &mut |e| events.push(e));
        }
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].code, KeyCode::Enter);
    }

    /// A frame reads back to the scan it was made of, and the six bits
    /// past the matrix are dropped.
    #[test]
    fn a_frame_reads_back_and_drops_the_bits_past_the_matrix() {
        let scan = down(&[(0, 0), H, (4, 9)]);
        assert_eq!(Scan::from_frame(&scan.frame()), scan);
        assert_eq!(scan.frame(), [0x01, 0x80, 0x00, 0x00, 0x00, 0x00, 0x02]);
        assert_eq!(Scan::from_frame(&[0, 0, 0, 0, 0, 0, 0xfc]), Scan::NONE);
    }

    /// The recording format: comments and the firmware's other lines are
    /// skipped, a malformed `KB` line is named by its number.
    #[test]
    fn a_recording_reads_and_names_its_bad_line() {
        let text = "# test\n[FW_BUILD] x\nKB 10 00800000000000\n\nKB 20 0080000000000\n";
        let read: Vec<_> = parse_recording(text).collect();
        assert_eq!(read[0], Ok((10, down(&[H]))));
        assert_eq!(read[1], Err(RecordingError::Line(5)));
        assert_eq!(read.len(), 2);
    }
}
