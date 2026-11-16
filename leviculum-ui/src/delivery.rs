//! The delivery glyph table: the one place that maps a message's delivery
//! state to its glyph, its colour, its weight and its ASCII fallback. The
//! ladder is `docs/src/concepts/loquax-read-receipts.md`, section 3 (1232):
//!
//! | state | glyph | ASCII | tone |
//! |---|---|---|---|
//! | queued, stamping, in flight, sent without proof | `◷ ` or the spinner | `. ` or `* ` | grey |
//! | in the mailbox | `✓ ` | `v ` | grey |
//! | arrived (proof: the recipient's mailbox or device) | `✓ ` | `v ` | grey |
//! | delivered (on the recipient's device, `lev` 0x07) | `✓✓` | `vv` | grey |
//! | read | `✔✔` bold | `VV` | green |
//! | rejected, failed | `! ` bold | `! ` | red |
//!
//! One table, so the conversation glyph, the detail view and anything else
//! that shows delivery read the same row and cannot disagree. loquax is its
//! first user; lnomad does not draw delivery. The rules the table encodes:
//! a tick only for what someone else confirmed (the mailbox or the
//! recipient's proof one, the recipient's device two: 1462, a proof is
//! signed by the recipient's identity, which its mailbox holds too, so it
//! cannot say "on the device"), green only for read, and read is more than its
//! colour: a heavier glyph in bold, so a reader who cannot tell green from
//! grey, or a monochrome terminal, still sees it. Unproven sent shares the
//! clock with sending: nobody confirmed it, and the detail view says it in
//! words. Every glyph is two cells wide, so the text before it never moves
//! when the state changes. Animation means the machine is working right
//! now, driven by the program's tick, so it freezes honestly if the program
//! hangs.
//!
//! The colours are tones, not RGB: the frontend resolves a tone against its
//! palette.

/// Where a sent message stands, as the reader is told it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DeliveryState {
    /// Queued, nothing transmitted yet.
    Queued,
    /// Computing the proof-of-work stamp.
    Stamping,
    /// In flight: a link is being built or the bytes are going out.
    InFlight,
    /// Handed to the network with no proof back (an opportunistic `Sent`).
    Sent,
    /// Left in a propagation node's mailbox (a propagated `Sent`).
    InMailbox,
    /// Arrived: the recipient's identity proved it. Its mailbox signs
    /// with that identity as its devices do, so this says "it reached the
    /// recipient's mailbox or device", never which (1462). One tick.
    Arrived,
    /// On the recipient's device: the device said so with its `lev`
    /// receipt `0x07` (1462). Two ticks.
    Delivered,
    /// Read: the peer's signed read receipt named it. `at` is the reader's
    /// clock when it came on their screen, unix seconds, kept within the
    /// time it was sent and the time the receipt came. Read implies
    /// delivered, and nothing moves a message on from it.
    Read { at: u32 },
    /// Refused, by the recipient or by a propagation node.
    Rejected,
    /// Given up.
    Failed,
}

/// A colour family, resolved against the frontend's palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tone {
    /// Grey, the secondary text's: on its way, in the mailbox, arrived,
    /// delivered.
    Gray,
    /// Green, the accent: read.
    Green,
    /// Red: not sent.
    Red,
}

/// The cells every glyph of the column takes.
pub const CELLS: usize = 2;

/// What a row draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mark {
    /// A fixed glyph, padded to [`CELLS`].
    Glyph(&'static str),
    /// The braille spinner of [`crate::spinner`], one frame per tick, padded
    /// to [`CELLS`].
    Spinner,
}

/// One row of the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DeliveryGlyph {
    /// The state this row presents.
    pub state: DeliveryState,
    /// What it draws on a terminal that renders the glyphs.
    pub mark: Mark,
    /// The fallback for terminals that mangle them, drawn static.
    pub ascii: &'static str,
    /// Its colour family.
    pub tone: Tone,
    /// Whether it is drawn bold: read and not sent.
    pub bold: bool,
}

/// The frames of [`crate::spinner::SPIN_FRAMES`], each padded to [`CELLS`].
const SPIN_PADDED: [&str; 6] = ["⠋ ", "⠙ ", "⠹ ", "⠸ ", "⠼ ", "⠴ "];

impl DeliveryGlyph {
    /// Whether the row animates on the tick.
    pub fn animated(&self) -> bool {
        self.mark == Mark::Spinner
    }

    /// The symbol to draw at animation step `spin`, or the ASCII fallback,
    /// [`CELLS`] wide either way.
    pub fn symbol(&self, spin: usize, ascii: bool) -> &'static str {
        match (ascii, self.mark) {
            (true, _) => self.ascii,
            (false, Mark::Glyph(glyph)) => glyph,
            (false, Mark::Spinner) => SPIN_PADDED[spin % SPIN_PADDED.len()],
        }
    }
}

/// The table, one row per [`DeliveryState`], in the order of the ladder.
pub const DELIVERY_GLYPHS: [DeliveryGlyph; 10] = [
    row(
        DeliveryState::Queued,
        Mark::Glyph("◷ "),
        ". ",
        Tone::Gray,
        false,
    ),
    row(
        DeliveryState::Stamping,
        Mark::Spinner,
        "* ",
        Tone::Gray,
        false,
    ),
    row(
        DeliveryState::InFlight,
        Mark::Spinner,
        "* ",
        Tone::Gray,
        false,
    ),
    row(
        DeliveryState::Sent,
        Mark::Glyph("◷ "),
        ". ",
        Tone::Gray,
        false,
    ),
    row(
        DeliveryState::InMailbox,
        Mark::Glyph("✓ "),
        "v ",
        Tone::Gray,
        false,
    ),
    row(
        DeliveryState::Arrived,
        Mark::Glyph("✓ "),
        "v ",
        Tone::Gray,
        false,
    ),
    row(
        DeliveryState::Delivered,
        Mark::Glyph("✓✓"),
        "vv",
        Tone::Gray,
        false,
    ),
    row(
        DeliveryState::Read { at: 0 },
        Mark::Glyph("✔✔"),
        "VV",
        Tone::Green,
        true,
    ),
    row(
        DeliveryState::Rejected,
        Mark::Glyph("! "),
        "! ",
        Tone::Red,
        true,
    ),
    row(
        DeliveryState::Failed,
        Mark::Glyph("! "),
        "! ",
        Tone::Red,
        true,
    ),
];

const fn row(
    state: DeliveryState,
    mark: Mark,
    ascii: &'static str,
    tone: Tone,
    bold: bool,
) -> DeliveryGlyph {
    DeliveryGlyph {
        state,
        mark,
        ascii,
        tone,
        bold,
    }
}

/// The row for `state`.
pub fn glyph(state: DeliveryState) -> &'static DeliveryGlyph {
    match state {
        DeliveryState::Queued => &DELIVERY_GLYPHS[0],
        DeliveryState::Stamping => &DELIVERY_GLYPHS[1],
        DeliveryState::InFlight => &DELIVERY_GLYPHS[2],
        DeliveryState::Sent => &DELIVERY_GLYPHS[3],
        DeliveryState::InMailbox => &DELIVERY_GLYPHS[4],
        DeliveryState::Arrived => &DELIVERY_GLYPHS[5],
        DeliveryState::Delivered => &DELIVERY_GLYPHS[6],
        DeliveryState::Read { .. } => &DELIVERY_GLYPHS[7],
        DeliveryState::Rejected => &DELIVERY_GLYPHS[8],
        DeliveryState::Failed => &DELIVERY_GLYPHS[9],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spinner;

    #[test]
    fn every_state_reads_its_own_row() {
        for row in &DELIVERY_GLYPHS {
            assert_eq!(glyph(row.state), row, "{:?}", row.state);
        }
        // Read's row is one, whenever it was read.
        assert_eq!(
            glyph(DeliveryState::Read { at: 1_800_000_000 }).state,
            DeliveryState::Read { at: 0 }
        );
    }

    /// The ladder of loquax-read-receipts.md, section 3, row by row.
    #[test]
    fn the_ladder_is_the_documented_table() {
        let got: Vec<(DeliveryState, &str, &str, Tone, bool)> = DELIVERY_GLYPHS
            .iter()
            .map(|r| (r.state, r.symbol(0, false), r.ascii, r.tone, r.bold))
            .collect();
        use DeliveryState as S;
        assert_eq!(
            got,
            [
                (S::Queued, "◷ ", ". ", Tone::Gray, false),
                (S::Stamping, "⠋ ", "* ", Tone::Gray, false),
                (S::InFlight, "⠋ ", "* ", Tone::Gray, false),
                (S::Sent, "◷ ", ". ", Tone::Gray, false),
                (S::InMailbox, "✓ ", "v ", Tone::Gray, false),
                (S::Arrived, "✓ ", "v ", Tone::Gray, false),
                (S::Delivered, "✓✓", "vv", Tone::Gray, false),
                (S::Read { at: 0 }, "✔✔", "VV", Tone::Green, true),
                (S::Rejected, "! ", "! ", Tone::Red, true),
                (S::Failed, "! ", "! ", Tone::Red, true),
            ]
        );
    }

    #[test]
    fn a_tick_means_confirmed_and_green_means_read() {
        for row in &DELIVERY_GLYPHS {
            let confirmed = matches!(
                row.state,
                DeliveryState::InMailbox
                    | DeliveryState::Arrived
                    | DeliveryState::Delivered
                    | DeliveryState::Read { .. }
            );
            let read = matches!(row.state, DeliveryState::Read { .. });
            let ticks = row
                .symbol(0, false)
                .chars()
                .filter(|c| matches!(c, '✓' | '✔'))
                .count();
            assert_eq!(ticks > 0, confirmed, "{:?}", row.state);
            assert_eq!(row.tone == Tone::Green, read, "{:?}", row.state);
            // Read is told by more than its colour.
            assert_eq!(row.symbol(0, false).contains('✔'), read, "{:?}", row.state);
        }
        // Two ticks only from the recipient's device: a proof is one
        // (1462), the mailbox can sign it.
        assert_eq!(glyph(DeliveryState::InMailbox).symbol(0, false), "✓ ");
        assert_eq!(glyph(DeliveryState::Arrived).symbol(0, false), "✓ ");
        assert_eq!(glyph(DeliveryState::Delivered).symbol(0, false), "✓✓");
    }

    #[test]
    fn every_glyph_is_two_cells_at_every_tick() {
        for row in &DELIVERY_GLYPHS {
            for spin in 0..12 {
                for ascii in [false, true] {
                    let symbol = row.symbol(spin, ascii);
                    assert_eq!(symbol.chars().count(), CELLS, "{:?} {symbol:?}", row.state);
                }
            }
        }
    }

    #[test]
    fn only_working_states_animate_and_they_use_the_shared_spinner() {
        for row in &DELIVERY_GLYPHS {
            let working = matches!(row.state, DeliveryState::Stamping | DeliveryState::InFlight);
            assert_eq!(row.animated(), working, "{:?}", row.state);
            if working {
                for spin in 0..12 {
                    let frame = row.symbol(spin, false);
                    assert_eq!(frame.trim_end(), spinner::glyph(spin));
                }
                // The fallback does not animate.
                assert_eq!(row.symbol(0, true), row.symbol(1, true));
            }
        }
    }

    #[test]
    fn the_ascii_fallback_is_the_documented_set() {
        let set: Vec<&str> = DELIVERY_GLYPHS.iter().map(|r| r.ascii).collect();
        assert_eq!(
            set,
            [". ", "* ", "* ", ". ", "v ", "v ", "vv", "VV", "! ", "! "]
        );
        for row in &DELIVERY_GLYPHS {
            assert!(row.ascii.is_ascii(), "{:?}", row.state);
        }
    }
}
