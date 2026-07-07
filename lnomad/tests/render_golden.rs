//! Golden + layout tests for the render refactor.
//!
//! The golden test is the byte-identity guard for the `--print` / non-tty path:
//! the two golden files were captured from the renderer BEFORE the
//! target-agnostic-styled-lines refactor, so an exact match proves the refactor
//! changed no output bytes. The layout tests exercise the new intermediate
//! (`layout` -> `RLine` + positioned `RenderedLink`) directly.

use leviculum_micron::parse;
use lnomad::color::ColorDepth;
use lnomad::render::{layout, render_with_options};
use lnomad::theme::Theme;

const SAMPLE: &str = include_str!("fixtures/sample.mu");
const GOLDEN_COLOR: &str = include_str!("fixtures/golden_80_color.ansi");
const GOLDEN_PLAIN: &str = include_str!("fixtures/golden_80_plain.txt");

#[test]
fn print_output_is_byte_identical_to_golden_color() {
    let doc = parse(SAMPLE);
    // Pinned to true colour so the frozen golden stays 24-bit `38;2;r;g;b`.
    let page = render_with_options(&doc, 80, false, ColorDepth::Truecolor);
    assert_eq!(
        page.text, GOLDEN_COLOR,
        "--print (colour) output drifted from the frozen golden"
    );
}

#[test]
fn print_output_is_byte_identical_to_golden_plain() {
    let doc = parse(SAMPLE);
    let page = render_with_options(&doc, 80, true, ColorDepth::Truecolor);
    assert_eq!(
        page.text, GOLDEN_PLAIN,
        "--print (no_color) output drifted from the frozen golden"
    );
}

#[test]
fn narrower_width_produces_more_lines() {
    let doc = parse(SAMPLE);
    let (wide, _) = layout(&doc, 80, Theme::Dark);
    let (narrow, _) = layout(&doc, 40, Theme::Dark);
    // The plain paragraph rewraps: 40 columns needs strictly more rows than 80.
    assert!(
        narrow.len() > wide.len(),
        "expected rewrap to add lines: 80 -> {} rows, 40 -> {} rows",
        wide.len(),
        narrow.len()
    );
}

/// Visible text of an `RLine` (drops style), for locating a laid-out link.
fn line_text(line: &lnomad::render::RLine) -> String {
    line.cells.iter().map(|c| c.ch).collect()
}

#[test]
fn bullet_link_records_its_laid_out_position() {
    let doc = parse(SAMPLE);
    let (lines, links) = layout(&doc, 80, Theme::Dark);

    // The plain "• Alpha" link is link 1; its label starts at column 0 (no
    // leading whitespace) and the clickable span covers just "• Alpha" (no
    // visible `[N]` marker is appended).
    let alpha = links
        .iter()
        .find(|l| l.target == ":/page/alpha.mu")
        .expect("alpha link");
    let alpha_line = &lines[alpha.line];
    assert!(
        line_text(alpha_line).starts_with("• Alpha"),
        "alpha not at line start: {:?}",
        line_text(alpha_line)
    );
    assert_eq!(alpha.col_start, 0, "alpha label should start at column 0");
    // "• Alpha" = 7 chars -> exclusive end 7.
    assert_eq!(alpha.col_end, 7, "alpha clickable span end");
    // Every cell in the recorded span is underline-styled (the clickable core).
    for ci in alpha.col_start..alpha.col_end {
        assert!(
            alpha_line.cells[ci].st.underline,
            "cell {ci} in alpha span not underlined"
        );
        assert_eq!(alpha_line.cells[ci].link, Some(alpha.index));
    }
}

#[test]
fn leading_whitespace_of_link_is_not_underlined() {
    let doc = parse(SAMPLE);
    let (lines, links) = layout(&doc, 80, Theme::Dark);

    // The "  • Beta" link's label carries two leading spaces; those must render
    // plain (not underlined, not tagged), and col_start must point past them.
    let beta = links
        .iter()
        .find(|l| l.target == ":/page/beta.mu")
        .expect("beta link");
    let beta_line = &lines[beta.line];
    assert_eq!(
        beta.col_start, 2,
        "col_start should skip the two leading spaces"
    );
    for ci in 0..beta.col_start {
        let leading = &beta_line.cells[ci];
        assert_eq!(leading.ch, ' ', "expected a leading space at {ci}");
        assert!(!leading.st.underline, "leading whitespace was underlined");
        assert_eq!(leading.link, None, "leading whitespace was tagged as link");
    }
    // The clickable core itself is underlined + tagged.
    assert!(beta_line.cells[beta.col_start].st.underline);
    assert_eq!(beta_line.cells[beta.col_start].link, Some(beta.index));
}
