//! The sim's window (1530): the panel at an integer scale, the host's
//! keyboard as the M9's, the page and the last `KEY` line in the title.
//! winit and softbuffer, the crates the GUI's window stands on.
//!
//! The keys: the arrows are the ring, Enter its centre, Esc Back,
//! Backspace erases, F1 to F5 the bubbles, the house, the square bubble,
//! the pin and the map, and every printable character its own key on the
//! QWERTY block (`+`, `-`, `t`, `y` the map's). Each becomes the
//! controller's code and goes the board's way ([`Board::press`]). A key
//! the host repeats is pressed again, as the bench's held key is.

use std::num::NonZeroU32;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use loquax_m9::frame::{HEIGHT, WIDTH};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

use crate::board::{self, Board};

/// How often the board turns while no key comes: the map's words go by
/// the clock.
const TICK: Duration = Duration::from_millis(50);

struct App<'a> {
    board: Board,
    scale: u32,
    save: Option<&'a Path>,
    start: Instant,
    window: Option<Rc<Window>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
    shown_frames: u64,
    error: Option<String>,
}

/// The controller's code for a host key.
fn code(key: &Key) -> Option<u8> {
    match key {
        Key::Named(named) => Some(match named {
            NamedKey::ArrowUp => 0xb5,
            NamedKey::ArrowDown => 0xb6,
            NamedKey::ArrowLeft => 0xb4,
            NamedKey::ArrowRight => 0xb7,
            NamedKey::Enter => 0x0d,
            NamedKey::Escape => 0x86,
            NamedKey::Backspace => 0x08,
            NamedKey::Space => 0x20,
            NamedKey::F1 => 0x81,
            NamedKey::F2 => 0x82,
            NamedKey::F3 => 0x83,
            NamedKey::F4 => 0x84,
            NamedKey::F5 => 0x85,
            _ => return None,
        }),
        Key::Character(text) => {
            let mut chars = text.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if c.is_ascii_graphic() || c == ' ' => Some(c as u8),
                _ => None,
            }
        }
        _ => None,
    }
}

impl App<'_> {
    fn title(&self) -> String {
        format!(
            "Loquax M9 sim  {}  {}",
            self.board.last_page.as_deref().unwrap_or(""),
            self.board.last_key.as_deref().unwrap_or("")
        )
    }

    fn turn(&mut self) {
        board::set_clock_ms(self.start.elapsed().as_millis() as u64);
        self.board.settle();
        if self.board.frames != self.shown_frames {
            if let Some(window) = &self.window {
                window.set_title(&self.title());
                window.request_redraw();
            }
        }
    }

    fn paint(&mut self) -> Result<(), String> {
        let (Some(window), Some(surface)) = (&self.window, self.surface.as_mut()) else {
            return Ok(());
        };
        let size = window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else {
            return Ok(());
        };
        surface
            .resize(w, h)
            .map_err(|e| format!("the window's surface: {e}"))?;
        let mut buffer = surface
            .buffer_mut()
            .map_err(|e| format!("the window's buffer: {e}"))?;
        buffer.fill(0);
        let rgb = self.board.frame().to_rgb888();
        let scale = self.scale;
        let (bw, bh) = (size.width, size.height);
        for y in 0..(HEIGHT * scale).min(bh) {
            for x in 0..(WIDTH * scale).min(bw) {
                let i = (((y / scale) * WIDTH + x / scale) * 3) as usize;
                let pixel = (u32::from(rgb[i]) << 16)
                    | (u32::from(rgb[i + 1]) << 8)
                    | u32::from(rgb[i + 2]);
                buffer[(y * bw + x) as usize] = pixel;
            }
        }
        buffer
            .present()
            .map_err(|e| format!("the window's present: {e}"))?;
        self.shown_frames = self.board.frames;
        Ok(())
    }
}

impl ApplicationHandler for App<'_> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attributes = Window::default_attributes()
            .with_title(self.title())
            .with_inner_size(PhysicalSize::new(WIDTH * self.scale, HEIGHT * self.scale))
            .with_resizable(false);
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Rc::new(window),
            Err(e) => {
                self.error = Some(format!("no window: {e}"));
                event_loop.exit();
                return;
            }
        };
        let surface = softbuffer::Context::new(window.clone())
            .and_then(|context| softbuffer::Surface::new(&context, window.clone()));
        match surface {
            Ok(surface) => self.surface = Some(surface),
            Err(e) => {
                self.error = Some(format!("no surface: {e}"));
                event_loop.exit();
                return;
            }
        }
        window.request_redraw();
        self.window = Some(window);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => {
                if let Err(e) = self.paint() {
                    self.error = Some(e);
                    event_loop.exit();
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                if let Some(code) = code(&logical_key) {
                    board::set_clock_ms(self.start.elapsed().as_millis() as u64);
                    self.board.press(code);
                    self.turn();
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.turn();
        event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + TICK));
    }
}

/// The window until it is closed; the flash saved to `save` then.
pub fn run(board: Board, scale: u32, save: Option<&Path>) -> Result<(), String> {
    let event_loop = EventLoop::new().map_err(|e| format!("no event loop: {e}"))?;
    let mut app = App {
        board,
        scale,
        save,
        start: Instant::now(),
        window: None,
        surface: None,
        shown_frames: u64::MAX,
        error: None,
    };
    event_loop
        .run_app(&mut app)
        .map_err(|e| format!("the event loop: {e}"))?;
    if let Some(error) = app.error.take() {
        return Err(error);
    }
    if let Some(save) = app.save {
        app.board.power_off().0.save(save)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::SmolStr;

    #[test]
    fn the_hosts_keys_are_the_m9s() {
        assert_eq!(code(&Key::Named(NamedKey::ArrowDown)), Some(0xb6));
        assert_eq!(code(&Key::Named(NamedKey::Escape)), Some(0x86));
        assert_eq!(code(&Key::Named(NamedKey::F5)), Some(0x85));
        assert_eq!(code(&Key::Character(SmolStr::new("t"))), Some(b't'));
        assert_eq!(code(&Key::Character(SmolStr::new("+"))), Some(b'+'));
        assert_eq!(code(&Key::Character(SmolStr::new("ü"))), None);
        assert_eq!(code(&Key::Named(NamedKey::Tab)), None);
    }
}
