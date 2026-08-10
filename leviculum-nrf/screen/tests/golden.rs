//! Golden-frame tests: pixel-exact renders of the status screen on the
//! host, compared against checked-in ASCII frames ('#' = on, '.' = off).
//!
//! Regenerate after an intentional visual change with
//! `UPDATE_GOLDEN=1 cargo test -p leviculum-screen --target <host>` and
//! review the diff like any other code change.

use leviculum_screen::{BatteryStatus, DirtyRect, GnssStatus, MonoFb, StatusModel, T114Fb, V2Fb};

fn render_ascii<const W: usize, const H: usize, const N: usize>(fb: &MonoFb<W, H, N>) -> String {
    let mut out = String::with_capacity((W + 1) * H);
    for y in 0..H {
        for x in 0..W {
            out.push(if fb.pixel(x, y) { '#' } else { '.' });
        }
        out.push('\n');
    }
    out
}

fn check_golden(name: &str, actual: &str) {
    let path = format!("{}/tests/golden/{}.txt", env!("CARGO_MANIFEST_DIR"), name);
    if std::env::var("UPDATE_GOLDEN").is_ok() {
        std::fs::create_dir_all(format!("{}/tests/golden", env!("CARGO_MANIFEST_DIR"))).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("golden file {path} missing ({e}); run with UPDATE_GOLDEN=1"));
    assert_eq!(
        expected, actual,
        "golden mismatch for {name}; rendered frame:\n{actual}"
    );
}

/// T114 frame as the display-less fleet default renders it: both optional
/// features off, heartbeat on.
fn t114_model(heartbeat: bool) -> StatusModel<'static> {
    StatusModel {
        title: "leviculum T114",
        id_short: "fc06c10642",
        rx: 17,
        tx: 5,
        battery: BatteryStatus::FeatureOff,
        gnss: GnssStatus::FeatureOff,
        heartbeat,
    }
}

#[test]
fn golden_t114_normal_frame() {
    let mut fb = T114Fb::new();
    t114_model(true).paint(&mut fb, 120).unwrap();
    check_golden("t114_normal", &render_ascii(&fb));
}

#[test]
fn golden_v2_normal_frame() {
    // Full-feature Pocket V2 frame: battery reading, valid fix with
    // coordinates, heartbeat off. Locks the exact frame the historical
    // in-loop formatting produced before the painter was extracted.
    let model = StatusModel {
        title: "leviculum RAK4631",
        id_short: "fc06c10642",
        rx: 12345,
        tx: 678,
        battery: BatteryStatus::Data {
            percent: 87,
            voltage_mv: 4012,
        },
        gnss: GnssStatus::Data {
            sats: 7,
            valid: true,
            coords: Some((53.07516, 8.80777)),
        },
        heartbeat: false,
    };
    let mut fb = V2Fb::new();
    model.paint(&mut fb, 128).unwrap();
    check_golden("v2_normal", &render_ascii(&fb));
}

#[test]
fn golden_v2_no_data_frame() {
    // V2 with features compiled in but nothing heard yet: "Bat: --",
    // "GPS: 0 sat init", "(no fix)".
    let model = StatusModel {
        title: "leviculum RAK4631",
        id_short: "fc06c10642",
        rx: 0,
        tx: 0,
        battery: BatteryStatus::NoData,
        gnss: GnssStatus::NoData,
        heartbeat: true,
    };
    let mut fb = V2Fb::new();
    model.paint(&mut fb, 128).unwrap();
    check_golden("v2_no_data", &render_ascii(&fb));
}

#[test]
fn golden_long_name_truncation() {
    // 20 chars of FONT_6X10 fill the 120-px T114 buffer exactly; the
    // title below is 32 chars. Chars 21+ must clip at the right edge
    // (and chars 25+ never even reach the string). The golden frame
    // proves the last visible column belongs to char 20 and no pixel
    // wraps or panics.
    let model = StatusModel {
        title: "leviculum T114 EXTRALONGBOARDNAME",
        id_short: "fc06c10642",
        rx: 4294967295,
        tx: 4294967295,
        battery: BatteryStatus::FeatureOff,
        gnss: GnssStatus::FeatureOff,
        heartbeat: false,
    };
    let mut fb = T114Fb::new();
    model.paint(&mut fb, 120).unwrap();
    check_golden("t114_truncation", &render_ascii(&fb));
}

#[test]
fn dirty_rect_rx_update() {
    // rx 17 -> 18 changes exactly one glyph on line 3 ("RX: 17" ->
    // "RX: 18", char index 5). FONT_6X10 glyphs are 6 px wide at x =
    // 5*6 = 30..35, line 3 occupies y = 20..29. The diff must mark the
    // changed glyph's ink region and nothing else.
    let mut before = T114Fb::new();
    let mut after = T114Fb::new();
    t114_model(true).paint(&mut before, 120).unwrap();
    let mut m = t114_model(true);
    m.rx = 18;
    m.paint(&mut after, 120).unwrap();

    let dirty = after.diff(&before).expect("rx change must dirty the frame");
    assert!(
        dirty.x0 >= 30 && dirty.x1 <= 35,
        "dirty x-range {}..{} must stay inside the changed glyph cell 30..35",
        dirty.x0,
        dirty.x1
    );
    assert!(
        dirty.y0 >= 20 && dirty.y1 <= 29,
        "dirty y-range {}..{} must stay inside text line 3 (y 20..29)",
        dirty.y0,
        dirty.y1
    );
    // Pin the exact ink box of '7'->'8' in FONT_6X10 so silent font or
    // layout drift fails loudly rather than shrinking coverage.
    assert_eq!(
        dirty,
        DirtyRect {
            x0: 30,
            y0: 21,
            x1: 34,
            y1: 27
        }
    );
}

#[test]
fn dirty_rect_heartbeat_only() {
    // Heartbeat off -> on: the only difference is the 2x2 dot at
    // (width-3, 0) = (117, 0).
    let mut off = T114Fb::new();
    let mut on = T114Fb::new();
    t114_model(false).paint(&mut off, 120).unwrap();
    t114_model(true).paint(&mut on, 120).unwrap();

    let dirty = on.diff(&off).expect("heartbeat flip must dirty the frame");
    assert_eq!(
        dirty,
        DirtyRect {
            x0: 117,
            y0: 0,
            x1: 118,
            y1: 1
        }
    );
}

#[test]
fn dirty_rect_identical_frames() {
    let mut a = T114Fb::new();
    let mut b = T114Fb::new();
    t114_model(true).paint(&mut a, 120).unwrap();
    t114_model(true).paint(&mut b, 120).unwrap();
    assert_eq!(a.diff(&b), None);

    // And copy_from makes any two buffers diff-clean.
    let mut c = T114Fb::new();
    c.copy_from(&a);
    assert_eq!(a.diff(&c), None);
}

#[test]
fn frame_key_quantization() {
    // The key must ignore sub-visible jitter (coordinate noise below
    // 5dp, voltage noise below 100 mV) and react to visible changes.
    let base = StatusModel {
        title: "leviculum RAK4631",
        id_short: "fc06c10642",
        rx: 1,
        tx: 2,
        battery: BatteryStatus::Data {
            percent: 80,
            voltage_mv: 3950,
        },
        gnss: GnssStatus::Data {
            sats: 5,
            valid: true,
            coords: Some((53.075160, 8.807770)),
        },
        heartbeat: false,
    };
    let jitter = StatusModel {
        battery: BatteryStatus::Data {
            percent: 80,
            voltage_mv: 3999, // same 100 mV bucket (39 dV)
        },
        gnss: GnssStatus::Data {
            sats: 5,
            valid: true,
            coords: Some((53.075164, 8.807774)), // same 5dp bucket
        },
        ..base
    };
    assert_eq!(base.key(), jitter.key());

    let moved = StatusModel {
        gnss: GnssStatus::Data {
            sats: 5,
            valid: true,
            coords: Some((53.07526, 8.80777)),
        },
        ..base
    };
    assert_ne!(base.key(), moved.key());

    let beat = StatusModel {
        heartbeat: true,
        ..base
    };
    assert_ne!(base.key(), beat.key());
}
