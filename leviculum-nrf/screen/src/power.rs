//! The display's power decisions: what a button press asks for, and what
//! the display task does with a request (leviculum#413, rows 369 and 374 of
//! `docs/src/firmware/host-test-seams.md`).
//!
//! `leviculum-nrf/src/button.rs` times the press and `display.rs` drives the
//! panel; what they decide is here, so the pocket-press rule and the I2C
//! error rule are host tests instead of a thumb on a board.

/// The shortest press that turns a dark display on. Shorter ones are
/// ignored while it is off, so a press in a pocket cannot wake the screen
/// and burn the battery; turning it off is one quick tap.
pub const LONG_PRESS_MIN_MS: u64 = 2_000;

/// The display state a press of `press_ms` asks for, or `None` when it is
/// ignored: any press turns a lit display off, a press of at least
/// [`LONG_PRESS_MIN_MS`] turns a dark one on.
pub const fn press_target(currently_on: bool, press_ms: u64) -> Option<bool> {
    if currently_on {
        Some(false)
    } else if press_ms >= LONG_PRESS_MIN_MS {
        Some(true)
    } else {
        None
    }
}

/// Apply a power `request` to a display whose state is `*on`: `None` without
/// a change, so the panel is not commanded at all; otherwise the result of
/// `set`, with `*on` following the request only when `set` succeeded. A
/// failed write keeps the old state, so the next frame tick still knows
/// what the panel shows.
pub fn apply_power<E>(
    request: bool,
    on: &mut bool,
    set: impl FnOnce(bool) -> Result<(), E>,
) -> Option<Result<(), E>> {
    if request == *on {
        return None;
    }
    let result = set(request);
    if result.is_ok() {
        *on = request;
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_press_turns_a_lit_display_off() {
        assert_eq!(press_target(true, 0), Some(false));
        assert_eq!(press_target(true, 50), Some(false));
        assert_eq!(press_target(true, 10_000), Some(false));
    }

    #[test]
    fn a_dark_display_wakes_only_on_a_long_press() {
        assert_eq!(press_target(false, 0), None);
        assert_eq!(press_target(false, LONG_PRESS_MIN_MS - 1), None);
        assert_eq!(press_target(false, LONG_PRESS_MIN_MS), Some(true));
        assert_eq!(press_target(false, 60_000), Some(true));
    }

    #[test]
    fn an_unchanged_request_does_not_command_the_panel() {
        let mut on = true;
        let r = apply_power(true, &mut on, |_| -> Result<(), ()> {
            panic!("an unchanged request reached the panel")
        });
        assert_eq!(r, None);
        assert!(on);
    }

    #[test]
    fn a_changed_request_follows_the_panel_write() {
        let mut on = true;
        let mut asked = None;
        let r = apply_power(false, &mut on, |v| -> Result<(), ()> {
            asked = Some(v);
            Ok(())
        });
        assert_eq!(r, Some(Ok(())));
        assert_eq!(asked, Some(false));
        assert!(!on);
    }

    #[test]
    fn a_failed_panel_write_keeps_the_old_state() {
        let mut on = false;
        let r = apply_power(true, &mut on, |_| Err("i2c nack"));
        assert_eq!(r, Some(Err("i2c nack")));
        assert!(!on, "the panel still shows what it showed");
    }
}
