//! The JEDEC verdict the SolarNode's boot acts on (leviculum#413).

use leviculum_qspi_boot::{identify, wants_second_read, Identified, SILENT};

/// The P25Q16H the SolarNode declares.
const P25Q16H: [u8; 3] = [0x85, 0x60, 0x15];

#[test]
fn a_part_that_names_itself_is_expected() {
    assert!(!wants_second_read(P25Q16H));
    assert_eq!(identify(P25Q16H, None, P25Q16H), Identified::Expected);
}

#[test]
fn a_part_woken_by_the_first_read_is_expected_on_the_second() {
    assert!(wants_second_read(SILENT));
    assert_eq!(
        identify(SILENT, Some(P25Q16H), P25Q16H),
        Identified::Expected
    );
}

#[test]
fn two_silent_reads_are_no_answer() {
    assert_eq!(
        identify(SILENT, Some(SILENT), P25Q16H),
        Identified::NoAnswer
    );
    assert_eq!(Identified::NoAnswer.as_str(), "no-answer");
}

#[test]
fn a_foreign_part_is_unexpected_on_either_read() {
    let w25q16 = [0xEF, 0x40, 0x15];
    assert!(!wants_second_read(w25q16));
    assert_eq!(identify(w25q16, None, P25Q16H), Identified::UnexpectedPart);
    assert_eq!(
        identify(SILENT, Some(w25q16), P25Q16H),
        Identified::UnexpectedPart
    );
    assert_eq!(Identified::UnexpectedPart.as_str(), "unexpected-part");
}

#[test]
fn a_part_that_reads_all_ones_is_a_foreign_part_not_silence() {
    // A floating MISO pulled high, or a part held in reset: not the
    // undriven bus the bit-banged second opinion is for.
    assert_eq!(
        identify([0xFF; 3], None, P25Q16H),
        Identified::UnexpectedPart
    );
}
