use crate::core::{Color, Rectangle, Size};
use crate::graphics::compositor::{self, Information};
use crate::graphics::damage;
use crate::graphics::error::{self, Error};
use crate::graphics::{self, Shell, Viewport};
use crate::{Layer, Renderer, Settings, fast};

use std::collections::VecDeque;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

pub struct Compositor {
    context: softbuffer::Context<Box<dyn compositor::Display>>,
    settings: Settings,
}

pub struct Surface {
    window: softbuffer::Surface<
        Box<dyn compositor::Display>,
        Box<dyn compositor::Window>,
    >,
    clip_mask: tiny_skia::Mask,
    layer_stack: VecDeque<Vec<Layer>>,
    background_color: Color,
    max_age: u8,
}

impl crate::graphics::Compositor for Compositor {
    type Renderer = Renderer;
    type Surface = Surface;

    async fn with_backend(
        settings: graphics::Settings,
        display: impl compositor::Display,
        _compatible_window: impl compositor::Window,
        _shell: Shell,
        backend: Option<&str>,
    ) -> Result<Self, Error> {
        match backend {
            None | Some("tiny-skia") | Some("tiny_skia") => {
                Ok(new(settings.into(), display))
            }
            Some(backend) => Err(Error::GraphicsAdapterNotFound {
                backend: "tiny-skia",
                reason: error::Reason::DidNotMatch {
                    preferred_backend: backend.to_owned(),
                },
            }),
        }
    }

    fn create_renderer(&self) -> Self::Renderer {
        Renderer::new(
            self.settings.default_font,
            self.settings.default_text_size,
        )
    }

    fn create_surface<W: compositor::Window + Clone>(
        &mut self,
        window: W,
        width: u32,
        height: u32,
    ) -> Self::Surface {
        let window = softbuffer::Surface::new(
            &self.context,
            Box::new(window.clone()) as _,
        )
        .expect("Create softbuffer surface for window");

        let mut surface = Surface {
            window,
            clip_mask: tiny_skia::Mask::new(1, 1).expect("Create clip mask"),
            layer_stack: VecDeque::new(),
            background_color: Color::BLACK,
            max_age: 0,
        };

        if width > 0 && height > 0 {
            self.configure_surface(&mut surface, width, height);
        }

        surface
    }

    fn configure_surface(
        &mut self,
        surface: &mut Self::Surface,
        width: u32,
        height: u32,
    ) {
        surface
            .window
            .resize(
                NonZeroU32::new(width).expect("Non-zero width"),
                NonZeroU32::new(height).expect("Non-zero height"),
            )
            .expect("Resize surface");

        surface.clip_mask =
            tiny_skia::Mask::new(width, height).expect("Create clip mask");
        surface.layer_stack.clear();
    }

    fn information(&self) -> Information {
        Information {
            adapter: String::from("CPU"),
            backend: String::from("tiny-skia"),
        }
    }

    fn present(
        &mut self,
        renderer: &mut Self::Renderer,
        surface: &mut Self::Surface,
        viewport: &Viewport,
        background_color: Color,
        on_pre_present: impl FnOnce(),
    ) -> Result<(), compositor::SurfaceError> {
        present(
            renderer,
            surface,
            viewport,
            background_color,
            on_pre_present,
        )
    }

    fn screenshot(
        &mut self,
        renderer: &mut Self::Renderer,
        viewport: &Viewport,
        background_color: Color,
    ) -> Vec<u8> {
        screenshot(renderer, viewport, background_color)
    }
}

pub fn new(
    settings: Settings,
    display: impl compositor::Display,
) -> Compositor {
    #[allow(unsafe_code)]
    let context = softbuffer::Context::new(Box::new(display) as _)
        .expect("Create softbuffer context");

    Compositor { context, settings }
}

pub fn present(
    renderer: &mut Renderer,
    surface: &mut Surface,
    viewport: &Viewport,
    background_color: Color,
    on_pre_present: impl FnOnce(),
) -> Result<(), compositor::SurfaceError> {
    let physical_size = viewport.physical_size();

    let mut buffer = surface
        .window
        .buffer_mut()
        .map_err(|_| compositor::SurfaceError::Lost)?;

    let last_layers = {
        let age = buffer.age();

        surface.max_age = surface.max_age.max(age);
        surface.layer_stack.truncate(surface.max_age as usize);

        if age > 0 {
            surface.layer_stack.get(age as usize - 1)
        } else {
            None
        }
    };

    let damage = last_layers
        .and_then(|last_layers| {
            (surface.background_color == background_color).then(|| {
                damage::diff(
                    last_layers,
                    renderer.layers(),
                    |layer| vec![layer.bounds],
                    Layer::damage,
                )
            })
        })
        .unwrap_or_else(|| vec![Rectangle::with_size(viewport.logical_size())]);

    // Leviculum's patch (1455): a frame in which nothing changed, or only
    // the box `set_quiet` names, is not presented. softbuffer's present
    // hands the whole buffer to the display server even when nothing was
    // drawn into it (0.4.8, X11: `shm_put_image` of the whole window; Lew's
    // window at 1276x2099 logged `present=24 ms` twice a second while
    // nothing moved), and iced asks a redraw after every message. The buffer goes back unpresented, so its
    // age does not move, and the layer stack does not either: the next
    // frame is compared with what is on the screen, the quiet box's
    // change with it.
    if damage.is_empty() || is_quiet(&damage) {
        return Ok(());
    }

    // Leviculum's patch (1442): what changed against the frame on the
    // screen, the last one presented, for the display server; the damage
    // above is against the frame this buffer held, `age` frames back.
    let whole = Rectangle::with_size(viewport.logical_size());
    let shown = match surface.layer_stack.front() {
        Some(front) if surface.background_color == background_color => {
            if buffer.age() == 1 {
                damage.clone()
            } else {
                damage::diff(
                    front,
                    renderer.layers(),
                    |layer| vec![layer.bounds],
                    Layer::damage,
                )
            }
        }
        _ => vec![whole],
    };
    let shown = physical_damage(
        &damage::group(shown, whole),
        viewport.scale_factor(),
        physical_size.width,
        physical_size.height,
    );

    let mut shown = shown;
    {
        let (shift, damage) = plan(
            last_layers.map(Vec::as_slice),
            renderer.layers(),
            damage,
            viewport,
        );
        surface.layer_stack.push_front(renderer.layers().to_vec());
        surface.background_color = background_color;

        let mut pixels = tiny_skia::PixmapMut::from_bytes(
            bytemuck::cast_slice_mut(&mut buffer),
            physical_size.width,
            physical_size.height,
        )
        .expect("Create pixel map");

        if let Some(shift) = &shift {
            crate::scroll::apply(pixels.data_mut(), physical_size.width, shift);
            // What the display server takes: the moved rows too (an
            // empty list is the whole buffer already).
            let fast::Px { x0, y0, x1, y1 } = shift.pane;
            if !shown.is_empty() {
                shown.push([x0, y0, x1 - x0, y1 - y0]);
            }
        }

        renderer.draw(
            &mut pixels,
            &mut surface.clip_mask,
            viewport,
            &damage,
            background_color,
        );
    }

    on_pre_present();
    // Leviculum's patch (1442): the display server takes the rectangles
    // that changed, not the whole buffer (softbuffer sets one
    // `wl_surface.damage_buffer` or SHM put per rectangle). Nothing on
    // the buffer is presented whole.
    let presented = match shown.is_empty() {
        true => buffer.present(),
        false => buffer.present_with_damage(
            &shown
                .iter()
                .filter_map(|&[x, y, width, height]| {
                    Some(softbuffer::Rect {
                        x,
                        y,
                        width: NonZeroU32::new(width)?,
                        height: NonZeroU32::new(height)?,
                    })
                })
                .collect::<Vec<_>>(),
        ),
    };
    presented.map_err(|_| compositor::SurfaceError::Lost)?;
    let _ = PRESENTS.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

/// Leviculum's patch (1455): the frames presented so far, by every
/// surface of the process. A frame meter tells a present from a redraw
/// that changed nothing by whether this moved.
static PRESENTS: AtomicU64 = AtomicU64::new(0);

/// Leviculum's patch (1455): the rectangle whose change alone presents
/// no frame (a frame meter's own numbers), logical pixels.
static QUIET: Mutex<Option<Rectangle>> = Mutex::new(None);

/// Leviculum's patch (1455): the frames presented so far.
pub fn presents() -> u64 {
    PRESENTS.load(Ordering::Relaxed)
}

/// Leviculum's patch (1455): a change inside `quiet` alone presents no
/// frame; it is drawn with the next frame that changes anything else.
/// For a frame meter, whose numbers change with every frame it counts
/// and must never be the reason for one. `None` lifts it.
pub fn set_quiet(quiet: Option<Rectangle>) {
    *QUIET.lock().unwrap_or_else(PoisonError::into_inner) = quiet;
}

/// Leviculum's patch (1455): whether `damage` lies inside the quiet
/// rectangle, a pixel of rounding allowed.
pub fn is_quiet(damage: &[Rectangle]) -> bool {
    let quiet = *QUIET.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(quiet) = quiet else {
        return false;
    };
    let quiet = quiet.expand(1.0);
    !damage.is_empty()
        && damage.iter().all(|r| {
            r.x >= quiet.x
                && r.y >= quiet.y
                && r.x + r.width <= quiet.x + quiet.width
                && r.y + r.height <= quiet.y + quiet.height
        })
}

/// Leviculum's patch (1456): above this many damage rectangles a frame is
/// drawn whole. `Renderer::draw` draws every layer once per rectangle, and
/// every quad or paragraph a rectangle cuts through a clip mask. Measured
/// (loquax's window bench, every frame of every gesture at four sizes,
/// against the whole window's cost for the same content): two to four
/// rectangles over at most an eighth of the window cost 0.2 to 0.8 of a
/// whole frame; five and more cost 0.7 to 1.9 of it even below a fiftieth
/// of the window, 9 of it at the 26 to 60 of a scroll frame of a real chat.
pub const MAX_DAMAGE_RECTANGLES: usize = 4;

/// Leviculum's patch (1456): above this share of the window a frame's
/// damage is drawn as the whole window. Measured as above: three or four
/// rectangles over an eighth to a half of the window cost 1.2 to 1.3
/// whole frames, over more than a half 1.5 to 1.7.
pub const MAX_DAMAGE_SHARE: f32 = 0.125;

/// Leviculum's patch (1456): `damage`, grouped, as it is drawn: the whole
/// window `bounds` when it is more than [`MAX_DAMAGE_RECTANGLES`]
/// rectangles or covers more than [`MAX_DAMAGE_SHARE`] of the window,
/// else as it is.
pub fn whole_when_fragmented(
    damage: Vec<Rectangle>,
    bounds: Rectangle,
) -> Vec<Rectangle> {
    let area: f32 = damage
        .iter()
        .filter_map(|r| r.intersection(&bounds))
        .map(|r| r.width * r.height)
        .sum();

    if damage.len() > MAX_DAMAGE_RECTANGLES
        || area > MAX_DAMAGE_SHARE * bounds.width * bounds.height
    {
        vec![bounds]
    } else {
        damage
    }
}

/// Leviculum's patch (1468): how to draw a frame over the buffer that
/// holds `last` (none: unknown), where `damage` changed: when it would be
/// drawn whole and one layer's content only moved by whole pixels
/// ([`crate::scroll`]), that move and what is left to draw after it;
/// else no move and `damage`, grouped, as [`whole_when_fragmented`]
/// draws it.
pub fn plan(
    last: Option<&[Layer]>,
    current: &[Layer],
    damage: Vec<Rectangle>,
    viewport: &Viewport,
) -> (Option<crate::scroll::Shift>, Vec<Rectangle>) {
    let whole = Rectangle::with_size(viewport.logical_size());
    let plain = whole_when_fragmented(damage::group(damage, whole), whole);
    let size = viewport.physical_size();
    if let Some(last) = last
        && plain == [whole]
        && let Some(shift) = crate::scroll::find(
            last,
            current,
            viewport.scale_factor(),
            size.width,
            size.height,
        )
    {
        let damage = whole_when_fragmented(
            damage::group(shift.damage.clone(), whole),
            whole,
        );
        if damage != [whole] {
            return (Some(shift), damage);
        }
    }
    (None, plain)
}

/// Leviculum's patch (1442): logical `damage` as the buffer's pixels at
/// `scale`, each rectangle rounded outward to whole pixels and kept
/// within the `width` by `height` buffer, `[x, y, width, height]`; a
/// rectangle with no pixel inside is left out.
pub fn physical_damage(
    damage: &[Rectangle],
    scale: f32,
    width: u32,
    height: u32,
) -> Vec<[u32; 4]> {
    damage
        .iter()
        .filter_map(|r| {
            let r = *r * scale;
            let clamp = |v: f32, max: u32| v.clamp(0.0, max as f32) as u32;
            let x0 = clamp(r.x.floor(), width);
            let y0 = clamp(r.y.floor(), height);
            let x1 = clamp((r.x + r.width).ceil(), width);
            let y1 = clamp((r.y + r.height).ceil(), height);
            (x0 < x1 && y0 < y1).then_some([x0, y0, x1 - x0, y1 - y0])
        })
        .collect()
}

pub fn screenshot(
    renderer: &mut Renderer,
    viewport: &Viewport,
    background_color: Color,
) -> Vec<u8> {
    let size = viewport.physical_size();

    let mut offscreen_buffer: Vec<u32> =
        vec![0; size.width as usize * size.height as usize];

    let mut clip_mask = tiny_skia::Mask::new(size.width, size.height)
        .expect("Create clip mask");

    renderer.draw(
        &mut tiny_skia::PixmapMut::from_bytes(
            bytemuck::cast_slice_mut(&mut offscreen_buffer),
            size.width,
            size.height,
        )
        .expect("Create offscreen pixel map"),
        &mut clip_mask,
        viewport,
        &[Rectangle::with_size(Size::new(
            size.width as f32,
            size.height as f32,
        ))],
        background_color,
    );

    offscreen_buffer.iter().fold(
        Vec::with_capacity(offscreen_buffer.len() * 4),
        |mut acc, pixel| {
            const A_MASK: u32 = 0xFF_00_00_00;
            const R_MASK: u32 = 0x00_FF_00_00;
            const G_MASK: u32 = 0x00_00_FF_00;
            const B_MASK: u32 = 0x00_00_00_FF;

            let a = ((A_MASK & pixel) >> 24) as u8;
            let r = ((R_MASK & pixel) >> 16) as u8;
            let g = ((G_MASK & pixel) >> 8) as u8;
            let b = (B_MASK & pixel) as u8;

            acc.extend([r, g, b, a]);
            acc
        },
    )
}
