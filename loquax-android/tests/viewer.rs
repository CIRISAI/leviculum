//! The photo viewer's zoom (1269) through the JNI's `query`, in the bytes
//! `Core.zoom` writes: it answers without a session, the state it hands
//! back is the next gesture's, a pinch is anchored and clamped, a pan
//! stops at the edge, a double tap and Back toggle, a swipe down closes
//! at 1x only, and a gesture it does not know is refused by name.

use loquax_android::schema::{self, Done, ZoomShown};

mod support {
    pub mod java;
}
use support::java::{java_map, Java};

/// `Core.zoom(state, gesture, reals...)` as Java builds it.
fn ask(state: &ZoomShown, gesture: &str, reals: &[(&str, f64)]) -> Vec<u8> {
    let mut entries = vec![
        ("t", Java::Str("zoom")),
        ("view_w", Java::Real(state.view_w)),
        ("view_h", Java::Real(state.view_h)),
        ("picture_w", Java::Real(state.picture_w)),
        ("picture_h", Java::Real(state.picture_h)),
        ("scale", Java::Real(state.scale)),
        ("pan_x", Java::Real(state.pan_x)),
        ("pan_y", Java::Real(state.pan_y)),
        ("gesture", Java::Str(gesture)),
    ];
    entries.extend(reals.iter().map(|(name, x)| (*name, Java::Real(*x))));
    java_map(&entries)
}

fn zoom(state: &ZoomShown, gesture: &str, reals: &[(&str, f64)]) -> ZoomShown {
    let bytes = loquax_android::query(&ask(state, gesture, reals));
    let shown: ZoomShown = schema::decode(&bytes).expect("a ZoomShown");
    assert!(shown.ok, "{gesture}: {shown:?}");
    shown
}

/// `PhotoView.show`'s first state: a 1000 by 2000 screen, a 2000 by
/// 1000 picture, at 1x.
fn first() -> ZoomShown {
    let state = ZoomShown {
        view_w: 1000.0,
        view_h: 2000.0,
        picture_w: 2000.0,
        picture_h: 1000.0,
        scale: 1.0,
        ..ZoomShown::default()
    };
    zoom(&state, "none", &[])
}

#[test]
fn the_viewer_zooms_in_javas_bytes_without_a_session() {
    let rest = first();
    assert!(rest.at_rest);
    assert_eq!(
        (rest.left, rest.top, rest.width, rest.height),
        (0.0, 750.0, 1000.0, 500.0)
    );
    assert_eq!(rest.max_scale, 4.0);
    // A pinch to 2x about the centre, then a far pinch: held at 4x.
    let two = zoom(
        &rest,
        "pinch",
        &[("factor", 2.0), ("x", 500.0), ("y", 1000.0)],
    );
    assert_eq!(two.scale, 2.0);
    assert!(!two.at_rest);
    assert_eq!((two.room_left, two.room_right), (500.0, 500.0));
    let most = zoom(
        &two,
        "pinch",
        &[("factor", 9.0), ("x", 500.0), ("y", 1000.0)],
    );
    assert_eq!(most.scale, 4.0);
    // A pan past the edge moves to it and says how far.
    let panned = zoom(&two, "pan", &[("dx", 800.0), ("dy", 40.0)]);
    assert_eq!((panned.moved_x, panned.moved_y), (500.0, 0.0));
    assert_eq!(panned.left, 0.0);
    assert_eq!(panned.room_right, 0.0);
    // Zoomed, a swipe does not close and Back goes to 1x first.
    assert!(!zoom(&two, "swipe", &[("dx", 0.0), ("dy", 900.0)]).close);
    let back = zoom(&two, "back", &[]);
    assert!(back.at_rest && !back.close);
    assert!(zoom(&back, "back", &[]).close);
    assert!(zoom(&back, "swipe", &[("dx", 0.0), ("dy", 900.0)]).close);
    // A double tap toggles.
    let tapped = zoom(&back, "doubletap", &[("x", 900.0), ("y", 1000.0)]);
    assert_eq!(tapped.scale, 2.0);
    assert!(zoom(&tapped, "doubletap", &[("x", 1.0), ("y", 1.0)]).at_rest);
    // The phone turned: the zoom kept, held to the new screen.
    let mut turned = panned.clone();
    (turned.view_w, turned.view_h) = (2000.0, 1000.0);
    let turned = zoom(&turned, "resize", &[]);
    assert_eq!(turned.scale, 2.0);
    assert!(turned.left <= 0.0 && turned.left + turned.width >= 2000.0);
}

#[test]
fn a_gesture_the_viewer_does_not_know_is_refused_by_name() {
    let bytes = loquax_android::query(&ask(&first(), "spin", &[]));
    let done: Done = schema::decode(&bytes).expect("a Done");
    assert!(!done.ok);
    assert!(done.error.unwrap_or_default().contains("\"spin\""));
}
