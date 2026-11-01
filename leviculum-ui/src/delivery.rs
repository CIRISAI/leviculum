//! The delivery glyph table of `docs/src/concepts/loquax-ui.md`, section 2: the
//! one place that maps a message's delivery state to its glyph, its colour,
//! its motion and its ASCII fallback.
//!
//! One table, so the conversation glyph, the detail view and anything else
//! that shows delivery read the same row and cannot disagree. loquax is its
//! first user; lnomad does not draw delivery. The rules the table encodes:
//! green and a check mark appear only on proof ([`DeliveryState::Delivered`])
//! or on what implies it, the peer's read receipt ([`DeliveryState::Read`],
//! `docs/src/concepts/loquax-read-receipts.md`, whose glyph ladder replaces
//! this table's marks in a later slice);
//! the mailbox state is a blue house, not a second check; amber hollow `◇`
//! against green solid `✓` mirrors unproven against proven; animation means
//! the machine is working right now, driven by the program's tick, so it
//! freezes honestly if the program hangs.
//!
//! The colours are tones, not RGB: the frontend resolves a tone against its
//! palette.

use crate::spinner;

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
    /// Arrived: a delivery proof came back.
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
    /// Gray.
    Gray,
    /// Yellow.
    Yellow,
    /// Amber.
    Amber,
    /// Blue.
    Blue,
    /// Green.
    Green,
    /// Red.
    Red,
}

/// What a row draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mark {
    /// A fixed glyph.
    Glyph(&'static str),
    /// The braille spinner of [`crate::spinner`], one frame per tick.
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
    /// Whether the tone is drawn dim.
    pub dim: bool,
}

impl DeliveryGlyph {
    /// Whether the row animates on the tick.
    pub fn animated(&self) -> bool {
        self.mark == Mark::Spinner
    }

    /// The symbol to draw at animation step `spin`, or the ASCII fallback.
    pub fn symbol(&self, spin: usize, ascii: bool) -> &'static str {
        match (ascii, self.mark) {
            (true, _) => self.ascii,
            (false, Mark::Glyph(glyph)) => glyph,
            (false, Mark::Spinner) => spinner::glyph(spin),
        }
    }
}

/// The table, one row per [`DeliveryState`], in the order of
/// `loquax-ui.md`, section 2.
pub const DELIVERY_GLYPHS: [DeliveryGlyph; 9] = [
    row(
        DeliveryState::Queued,
        Mark::Glyph("○"),
        ".",
        Tone::Gray,
        true,
    ),
    row(
        DeliveryState::Stamping,
        Mark::Spinner,
        "*",
        Tone::Yellow,
        true,
    ),
    row(
        DeliveryState::InFlight,
        Mark::Spinner,
        "*",
        Tone::Yellow,
        false,
    ),
    row(
        DeliveryState::Sent,
        Mark::Glyph("◇"),
        "o",
        Tone::Amber,
        false,
    ),
    row(
        DeliveryState::InMailbox,
        Mark::Glyph("⌂"),
        "^",
        Tone::Blue,
        false,
    ),
    row(
        DeliveryState::Delivered,
        Mark::Glyph("✓"),
        "v",
        Tone::Green,
        false,
    ),
    row(
        DeliveryState::Read { at: 0 },
        Mark::Glyph("✔"),
        "V",
        Tone::Green,
        false,
    ),
    row(
        DeliveryState::Rejected,
        Mark::Glyph("✗"),
        "x",
        Tone::Red,
        false,
    ),
    row(
        DeliveryState::Failed,
        Mark::Glyph("✗"),
        "x",
        Tone::Red,
        true,
    ),
];

const fn row(
    state: DeliveryState,
    mark: Mark,
    ascii: &'static str,
    tone: Tone,
    dim: bool,
) -> DeliveryGlyph {
    DeliveryGlyph {
        state,
        mark,
        ascii,
        tone,
        dim,
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
        DeliveryState::Delivered => &DELIVERY_GLYPHS[5],
        DeliveryState::Read { .. } => &DELIVERY_GLYPHS[6],
        DeliveryState::Rejected => &DELIVERY_GLYPHS[7],
        DeliveryState::Failed => &DELIVERY_GLYPHS[8],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn green_and_a_check_mark_mean_proof_and_nothing_else() {
        for row in &DELIVERY_GLYPHS {
            let proof = row.state == DeliveryState::Delivered;
            let read = matches!(row.state, DeliveryState::Read { .. });
            assert_eq!(row.tone == Tone::Green, proof || read, "{:?}", row.state);
            assert_eq!(row.symbol(0, false) == "✓", proof, "{:?}", row.state);
            assert_eq!(row.symbol(0, true) == "v", proof, "{:?}", row.state);
            assert_eq!(row.symbol(0, false) == "✔", read, "{:?}", row.state);
        }
    }

    #[test]
    fn only_working_states_animate_and_they_use_the_shared_spinner() {
        for row in &DELIVERY_GLYPHS {
            let working = matches!(row.state, DeliveryState::Stamping | DeliveryState::InFlight);
            assert_eq!(row.animated(), working, "{:?}", row.state);
            if working {
                for spin in 0..12 {
                    assert_eq!(row.symbol(spin, false), spinner::glyph(spin));
                }
                // The fallback does not animate.
                assert_eq!(row.symbol(0, true), row.symbol(1, true));
            }
        }
    }

    #[test]
    fn the_ascii_fallback_is_the_documented_set() {
        let set: Vec<&str> = DELIVERY_GLYPHS.iter().map(|r| r.ascii).collect();
        assert_eq!(set, [".", "*", "*", "o", "^", "v", "V", "x", "x"]);
        for row in &DELIVERY_GLYPHS {
            assert!(row.ascii.is_ascii(), "{:?}", row.state);
        }
    }
}
