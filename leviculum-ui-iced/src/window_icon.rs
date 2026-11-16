//! The suite's window icon, the jackdaw (1336): the picture a desktop
//! shows for the window in its task bar and switcher. Rendered from the
//! one geometry by `loquax-android/scripts/render-icon.py`, which writes
//! `assets/window-icon.png`; a program passes [`window_icon`] to its
//! `window::Settings`.

use iced::window::{icon, Icon};

/// The icon's PNG, 128 x 128, 8-bit RGBA.
const PNG: &[u8] = include_bytes!("../assets/window-icon.png");

/// The window icon, or `None` when the PNG does not decode to RGBA: a
/// window without its icon still opens.
pub fn window_icon() -> Option<Icon> {
    let mut reader = png::Decoder::new(std::io::Cursor::new(PNG))
        .read_info()
        .ok()?;
    let mut rgba = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut rgba).ok()?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    rgba.truncate(info.buffer_size());
    icon::from_rgba(rgba, info.width, info.height).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed PNG is what `from_rgba` takes: a window that asks for
    /// the icon gets one, at the size the script renders.
    #[test]
    fn the_window_icon_decodes_at_its_rendered_size() {
        let icon = window_icon().expect("assets/window-icon.png decodes to RGBA");
        let (_, size) = icon.into_raw();
        assert_eq!((size.width, size.height), (128, 128));
    }
}
