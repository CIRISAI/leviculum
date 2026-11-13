//! The read rule (1529): a message is read once its end was on screen,
//! not when it was selected or half of it showed. The terminal counts in
//! rows (`loquax::ui::pager::last_line_shown`, the terminal's and the
//! M9's viewport), the window in pixels (`leviculum_ui_iced::rows`, its
//! chat's viewport, 1457): one rule for both, so the targets mark the same
//! messages read.

use core::ops::Add;

/// Whether the end of what spans `start..end` lies in the view
/// `top..top + shown`: it draws something, its end is below the view's
/// top, and its last unit is within the view.
pub fn end_shown<T>(start: T, end: T, top: T, shown: T) -> bool
where
    T: Copy + PartialOrd + Add<Output = T>,
{
    start < end && end > top && end <= top + shown
}

#[cfg(test)]
mod tests {
    use super::end_shown;

    /// The rule in rows is the terminal's (`end - 1 < top + rows`), and in
    /// pixels the same: the end on the view's bottom edge is in, one past
    /// it is not; a span above the view is not; an empty one never is.
    #[test]
    fn the_end_must_be_in_the_view() {
        assert!(!end_shown(0usize, 43, 0, 10));
        assert!(!end_shown(0usize, 43, 32, 10));
        assert!(end_shown(0usize, 43, 33, 10));
        assert!(!end_shown(0usize, 0, 0, 10));
        assert!(end_shown(100.0f32, 180.0, 0.0, 180.0));
        assert!(!end_shown(100.0f32, 180.5, 0.0, 180.0));
        assert!(!end_shown(100.0f32, 180.0, 180.0, 300.0));
    }
}
