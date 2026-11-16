//! The desktop's light or dark, for a window that follows it (1310).
//!
//! iced asks winit, and winit knows the answer on Windows and macOS but not
//! on Linux, where a window on a light desktop then stays dark. There the
//! answer is the freedesktop colour scheme, read over D-Bus with mundy, the
//! reader iced's own `linux-theme-detection` uses. That feature blocks the
//! window's start for up to 200 ms on a portal that does not answer; this
//! runs the same stream as a subscription, so the window starts at once and
//! takes the desktop's tone when it comes.
//!
//! Both halves go together: [`theme`] at boot, [`theme_changes`] as a
//! subscription. On Linux the first is empty and the second's first item is
//! the answer, so a late winit `None` can never undo the desktop's tone.

use iced::theme::Mode;
use iced::{Subscription, Task};

/// The desktop's tone at boot: winit's off Linux, nothing on Linux (the
/// first item of [`theme_changes`] is the answer there).
pub fn theme() -> Task<Mode> {
    #[cfg(target_os = "linux")]
    {
        Task::none()
    }
    #[cfg(not(target_os = "linux"))]
    {
        iced::system::theme()
    }
}

/// The desktop's tone now and whenever it changes. On Linux the first item
/// comes once D-Bus answers; without a session bus or a portal it is
/// [`Mode::None`] and the stream ends.
pub fn theme_changes() -> Subscription<Mode> {
    #[cfg(target_os = "linux")]
    {
        Subscription::run(linux::stream)
    }
    #[cfg(not(target_os = "linux"))]
    {
        iced::system::theme_changes()
    }
}

#[cfg(target_os = "linux")]
pub mod linux {
    //! The freedesktop half, public for its test.

    use iced::futures::{Stream, StreamExt};
    use iced::theme::Mode;

    /// The colour scheme as a stream of modes: the current one first, then
    /// each change.
    pub fn stream() -> impl Stream<Item = Mode> {
        mundy::Preferences::stream(mundy::Interest::ColorScheme)
            .map(|preferences| mode(preferences.color_scheme))
    }

    fn mode(scheme: mundy::ColorScheme) -> Mode {
        match scheme {
            mundy::ColorScheme::NoPreference => Mode::None,
            mundy::ColorScheme::Light => Mode::Light,
            mundy::ColorScheme::Dark => Mode::Dark,
        }
    }
}
