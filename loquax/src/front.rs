//! Which frontend a start opens (1342, Lew 2026-11-02): with no subcommand
//! the window when there is a graphical session to put it on and the
//! binary carries it, the terminal view otherwise. `loquax tui` and
//! `loquax gui` insist. A pure function over what was asked, whether a
//! display is there and whether the window is built in, so every case is a
//! test and not a desktop to sit at.

use std::ffi::OsString;

/// What the command line asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ask {
    /// No subcommand and none of the terminal's own flags: Loquax picks.
    Default,
    /// `loquax tui`, or no subcommand with a flag only the terminal view
    /// takes (`--script`, `--to`, `--ascii`, ...): whoever typed it means
    /// the terminal.
    Tui,
    /// `loquax gui`. Asked only where the subcommand is built, so only
    /// the tests ask it of a build without the window.
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    Gui,
}

/// What the start opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Front {
    /// The terminal view, with the line to say first on stderr, if any.
    Tui(Option<&'static str>),
    Gui,
    /// Nothing: the sentence that says why, and the run fails.
    Refuse(&'static str),
}

/// Said when Loquax could have opened its window and there is no display.
pub(crate) const NO_DISPLAY_NOTE: &str =
    "no display, starting the terminal view; \"loquax gui\" to insist";
/// Said when `loquax gui` is asked for and there is no display.
pub(crate) const NO_DISPLAY_REFUSAL: &str = "there is no display to open the window on: neither \
     WAYLAND_DISPLAY nor DISPLAY is set; \"loquax tui\" starts the terminal view";
/// Said when `loquax gui` is asked of a build without the window. clap
/// refuses the subcommand before this in such a build, so it is the
/// decision's answer for the case and not a line anyone reads.
pub(crate) const NO_WINDOW_REFUSAL: &str =
    "this loquax is built without its window; \"loquax tui\" starts the terminal view";

/// The messenger's top-level options that both frontends take. Any other
/// given without a subcommand is the terminal's own and asks for it.
pub(crate) const SHARED: &[&str] = &[
    "pn",
    "instance",
    "config",
    "timeout",
    "sync_interval",
    "verbose",
    "daemon",
];

/// Whether a graphical session is there: `WAYLAND_DISPLAY` or `DISPLAY`
/// set and not empty, the two winit opens a window on.
pub(crate) fn has_display(var: impl Fn(&str) -> Option<OsString>) -> bool {
    ["WAYLAND_DISPLAY", "DISPLAY"]
        .iter()
        .any(|name| var(name).is_some_and(|value| !value.is_empty()))
}

/// The frontend for `ask` with a display there or not, in a build with the
/// window or without it. A build without the window starts the terminal
/// view and says nothing about it: there was never a choice.
pub(crate) fn front(ask: Ask, display: bool, gui_built: bool) -> Front {
    match (ask, display, gui_built) {
        (Ask::Tui, _, _) => Front::Tui(None),
        (Ask::Default, _, false) => Front::Tui(None),
        (Ask::Default, true, true) => Front::Gui,
        (Ask::Default, false, true) => Front::Tui(Some(NO_DISPLAY_NOTE)),
        (Ask::Gui, _, false) => Front::Refuse(NO_WINDOW_REFUSAL),
        (Ask::Gui, true, true) => Front::Gui,
        (Ask::Gui, false, true) => Front::Refuse(NO_DISPLAY_REFUSAL),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every (ask, display, build) there is: the eight of plain `loquax`
    /// and `loquax tui`, and the four of `loquax gui`.
    #[test]
    fn every_start_opens_what_the_decision_says() {
        use Ask::*;
        let cases = [
            (Default, true, true, Front::Gui),
            (Default, false, true, Front::Tui(Some(NO_DISPLAY_NOTE))),
            (Default, true, false, Front::Tui(None)),
            (Default, false, false, Front::Tui(None)),
            (Tui, true, true, Front::Tui(None)),
            (Tui, false, true, Front::Tui(None)),
            (Tui, true, false, Front::Tui(None)),
            (Tui, false, false, Front::Tui(None)),
            (Gui, true, true, Front::Gui),
            (Gui, false, true, Front::Refuse(NO_DISPLAY_REFUSAL)),
            (Gui, true, false, Front::Refuse(NO_WINDOW_REFUSAL)),
            (Gui, false, false, Front::Refuse(NO_WINDOW_REFUSAL)),
        ];
        for (ask, display, built, want) in cases {
            assert_eq!(
                front(ask, display, built),
                want,
                "{ask:?}, display {display}, gui built {built}"
            );
        }
    }

    /// Either variable names a session; an empty one names none, as a
    /// `DISPLAY=` an ssh session or a unit file leaves behind.
    #[test]
    fn a_display_is_wayland_or_x11_and_not_empty() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| OsString::from(value))
            }
        };
        assert!(has_display(env(&[("WAYLAND_DISPLAY", "wayland-0")])));
        assert!(has_display(env(&[("DISPLAY", ":0")])));
        assert!(has_display(env(&[
            ("DISPLAY", ""),
            ("WAYLAND_DISPLAY", "w")
        ])));
        assert!(!has_display(env(&[])));
        assert!(!has_display(env(&[
            ("DISPLAY", ""),
            ("WAYLAND_DISPLAY", "")
        ])));
        assert!(!has_display(env(&[("XDG_SESSION_TYPE", "x11")])));
    }
}
