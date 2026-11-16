//! The photo viewer's zoom (1269): one gesture of [`schema::ZoomAsk`]
//! folded by `loquax::ui::zoom`, the window's state machine too, into a
//! [`schema::ZoomShown`]. The app holds the state between gestures and
//! hands it back; the platform only reports what the fingers did.

use loquax::ui::zoom::Zoom;

use crate::schema::{self, ZoomAsk, ZoomShown, VERSION};

/// `ask`'s gesture on its state; the state after it and where the
/// picture lands, or the gesture it does not know.
pub fn zoom(ask: &ZoomAsk) -> Result<ZoomShown, String> {
    let mut zoom = Zoom::from_parts(
        (ask.view_w, ask.view_h),
        (ask.picture_w, ask.picture_h),
        ask.scale,
        (ask.pan_x, ask.pan_y),
    );
    let mut close = false;
    let mut moved = (0.0, 0.0);
    match ask.gesture.as_str() {
        "pinch" => zoom.pinch(ask.factor, ask.x, ask.y),
        "pan" => moved = zoom.pan_by(ask.dx, ask.dy),
        "doubletap" => zoom.double_tap(ask.x, ask.y),
        "back" => close = zoom.back(),
        "swipe" => close = zoom.swipe_closes(ask.dx, ask.dy),
        // `from_parts` already took the new screen and held to it.
        "resize" | "none" => {}
        other => return Err(format!("{other:?} is no gesture of the viewer")),
    }
    let rect = zoom.rect();
    let room = zoom.room();
    Ok(ZoomShown {
        v: VERSION,
        ok: true,
        view_w: zoom.view().0,
        view_h: zoom.view().1,
        picture_w: zoom.picture().0,
        picture_h: zoom.picture().1,
        scale: zoom.scale(),
        pan_x: zoom.pan().0,
        pan_y: zoom.pan().1,
        left: rect.left,
        top: rect.top,
        width: rect.width,
        height: rect.height,
        room_left: room.left,
        room_right: room.right,
        room_up: room.up,
        room_down: room.down,
        max_scale: zoom.max_scale(),
        at_rest: zoom.at_rest(),
        close,
        moved_x: moved.0,
        moved_y: moved.1,
    })
}

/// [`zoom`] as the JNI answers it: a [`schema::Done`] with the error
/// for a gesture it does not know.
pub fn answer(ask: &ZoomAsk) -> Vec<u8> {
    match zoom(ask) {
        Ok(shown) => schema::encode(&shown),
        Err(error) => schema::encode(&schema::Done::failed(error)),
    }
}
