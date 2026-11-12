//! Geometry simplification for the cut, all integer so the output is the
//! same on every machine: Douglas-Peucker, quantisation to a coarser
//! extent, and the area test the thinning uses.

/// A vertex in tile units.
pub type Pt = [i32; 2];

/// Twice the squared distance test: whether `p` lies further than `tol`
/// from the line through `a` and `b` (from `a` when they coincide).
fn beyond(p: Pt, a: Pt, b: Pt, tol: i64) -> bool {
    let (dx, dy) = (delta(a[0], b[0]), delta(a[1], b[1]));
    let (px, py) = (delta(a[0], p[0]), delta(a[1], p[1]));
    let len2 = (dx * dx + dy * dy).unsigned_abs();
    let tol2 = u128::from(tol.unsigned_abs()).pow(2);
    if len2 == 0 {
        return (px * px + py * py).unsigned_abs() > tol2;
    }
    // cross^2 needs up to 132 bits at the extremes: one that does not fit
    // is beyond any tolerance.
    let cross = (dx * py - dy * px).unsigned_abs();
    cross
        .checked_mul(cross)
        .is_none_or(|c2| c2 > tol2.saturating_mul(len2))
}

/// `to - from`, widened (1727): two `i32` coordinates differ by up to
/// 2^32, and the product of two such differences needs 66 bits, so every
/// distance test computes in 128 bits. A source tile is free to hold any
/// `i32` coordinate.
fn delta(from: i32, to: i32) -> i128 {
    i128::from(to) - i128::from(from)
}

/// The inner vertex of `pts[s..=e]` farthest from the chord `s`-`e`
/// (from `s` where the chord is a point), the first of equals; `s` when
/// every inner vertex lies on the chord. Squared distances times the
/// chord's squared length are compared, exactly, as [`beyond`] tests.
fn farthest(pts: &[Pt], s: usize, e: usize) -> usize {
    let (a, b) = (pts[s], pts[e]);
    let (dx, dy) = (delta(a[0], b[0]), delta(a[1], b[1]));
    let mut best = (0u128, s);
    for (i, p) in pts.iter().enumerate().take(e).skip(s + 1) {
        let (px, py) = (delta(a[0], p[0]), delta(a[1], p[1]));
        let d = if dx == 0 && dy == 0 {
            (px * px + py * py).unsigned_abs()
        } else {
            let c = (dx * py - dy * px).unsigned_abs();
            c.saturating_mul(c)
        };
        if d > best.0 {
            best = (d, i);
        }
    }
    best.1
}

/// Douglas-Peucker over an open line: the end points stay, an inner
/// vertex stays when it is further than `tol` from the kept line.
pub fn douglas_peucker(pts: &[Pt], tol: i64) -> Vec<Pt> {
    if pts.len() <= 2 || tol <= 0 {
        return pts.to_vec();
    }
    let mut keep = vec![false; pts.len()];
    keep[0] = true;
    keep[pts.len() - 1] = true;
    let mut stack = vec![(0usize, pts.len() - 1)];
    while let Some((s, e)) = stack.pop() {
        if e <= s + 1 {
            continue;
        }
        // The farthest vertex, by the same exact comparison as the test.
        let (a, b) = (pts[s], pts[e]);
        let i = farthest(pts, s, e);
        if i > s && beyond(pts[i], a, b, tol) {
            keep[i] = true;
            stack.push((s, i));
            stack.push((i, e));
        }
    }
    pts.iter()
        .zip(keep)
        .filter_map(|(p, k)| k.then_some(*p))
        .collect()
}

/// What a joined end is held to after the cut (1707), in tile units: an
/// end that lies on another line in the source (within half a unit,
/// [`near_segment_half`]) stays within one unit of that line.
pub const HELD_TOL: i64 = 1;

/// Whether `p` lies within half a unit of the segment `a`-`b`: on the
/// segment's own span by the exact cross-product test, or on one of its
/// ends, which with integer coordinates means equal to it.
pub fn near_segment_half(p: Pt, a: Pt, b: Pt) -> bool {
    let (dx, dy) = (delta(a[0], b[0]), delta(a[1], b[1]));
    let (px, py) = (delta(a[0], p[0]), delta(a[1], p[1]));
    let len2 = dx * dx + dy * dy;
    let dot = px * dx + py * dy;
    if len2 == 0 || dot < 0 || dot > len2 {
        // The nearest point is a vertex, at least a whole unit away
        // unless it is the same point.
        return p == a || p == b;
    }
    // |cross| / len is the distance; within half a unit when
    // 4 cross^2 <= len^2. cross^2 needs up to 130 bits at the extremes,
    // so compare unsigned, saturating: a saturated side is far off.
    let cross = (dx * py - dy * px).unsigned_abs();
    cross
        .checked_mul(cross)
        .and_then(|c| c.checked_mul(4))
        .is_some_and(|c| c <= len2.unsigned_abs())
}

/// [`douglas_peucker`] with vertices held (1707). An `exact[i]` vertex
/// is dropped only when it lies exactly on the kept chord — the cut
/// marks the tile-edge strip so, where the neighbouring tile carries its
/// own copy of the line, and exact collinearity is the same in both
/// tiles, so the two copies trace the same source path there. An anchor
/// `(i, p)`, another line's end `p` resting on segment `i` in the
/// source, keeps the simplified line within [`HELD_TOL`] of `p`, so
/// ways that join in the source stay joined in the cut.
pub fn douglas_peucker_held(
    pts: &[Pt],
    tol: i64,
    exact: &[bool],
    anchors: &[(usize, Pt)],
) -> Vec<Pt> {
    if pts.len() <= 2 || tol <= 0 {
        return pts.to_vec();
    }
    let mut keep = vec![false; pts.len()];
    keep[0] = true;
    keep[pts.len() - 1] = true;
    let mut stack = vec![(0usize, pts.len() - 1)];
    while let Some((s, e)) = stack.pop() {
        if e <= s + 1 {
            continue;
        }
        // The farthest own vertex, as [`douglas_peucker`] finds it.
        let (a, b) = (pts[s], pts[e]);
        let i = farthest(pts, s, e);
        if i > s && beyond(pts[i], a, b, tol) {
            keep[i] = true;
            stack.push((s, i));
            stack.push((i, e));
            continue;
        }
        // The chord holds every own vertex within the tolerance; an
        // exact vertex off the chord splits the span at itself.
        if let Some(i) = (s + 1..e).find(|&i| exact[i] && beyond(pts[i], a, b, 0)) {
            keep[i] = true;
            stack.push((s, i));
            stack.push((i, e));
            continue;
        }
        // An anchor out of the chord's reach splits the span at the
        // anchor's own segment, until the chord there is the source
        // segment the anchor rests on.
        for &(j, p) in anchors {
            if j >= s && j < e && beyond(p, a, b, HELD_TOL) {
                let at = j.clamp(s + 1, e - 1);
                keep[at] = true;
                stack.push((s, at));
                stack.push((at, e));
                break;
            }
        }
    }
    pts.iter()
        .zip(keep)
        .filter_map(|(p, k)| k.then_some(*p))
        .collect()
}

/// Douglas-Peucker over a closed ring: anchored at the first vertex and at
/// the vertex farthest from it, so a ring never collapses to its start.
pub fn simplify_ring(ring: &[Pt], tol: i64) -> Vec<Pt> {
    let mut r = ring.to_vec();
    if r.len() > 1 && r.first() == r.last() {
        r.pop();
    }
    if r.len() <= 3 || tol <= 0 {
        return r;
    }
    let far = (1..r.len())
        .max_by_key(|&i| {
            let (dx, dy) = (i64::from(r[i][0] - r[0][0]), i64::from(r[i][1] - r[0][1]));
            // Ties to the first index: `max_by_key` keeps the last, so key
            // on the negated index second.
            (dx * dx + dy * dy, -(i as i64))
        })
        .unwrap_or(1);
    let mut first = douglas_peucker(&r[..=far], tol);
    let mut back: Vec<Pt> = r[far..].to_vec();
    back.push(r[0]);
    let second = douglas_peucker(&back, tol);
    first.extend_from_slice(&second[1..second.len() - 1]);
    first
}

/// `v` rounded to a grid `1 << k` times coarser, half away from minus
/// infinity: `floor((v + half) / step)`.
pub fn quantise(v: i32, k: u32) -> i32 {
    if k == 0 {
        return v;
    }
    (v + (1 << (k - 1))).div_euclid(1 << k)
}

/// The points quantised, consecutive repeats dropped; for a ring also a
/// last point equal to the first.
pub fn quantise_path(pts: &[Pt], k: u32, closed: bool) -> Vec<Pt> {
    let mut out: Vec<Pt> = Vec::with_capacity(pts.len());
    for p in pts {
        let q = [quantise(p[0], k), quantise(p[1], k)];
        if out.last() != Some(&q) {
            out.push(q);
        }
    }
    if closed && out.len() > 1 && out.first() == out.last() {
        out.pop();
    }
    out
}

/// Twice the signed area of a ring (shoelace), in square tile units.
pub fn area2(ring: &[Pt]) -> i64 {
    let n = ring.len();
    (0..n)
        .map(|i| {
            let (a, b) = (ring[i], ring[(i + 1) % n]);
            i64::from(a[0]) * i64::from(b[1]) - i64::from(b[0]) * i64::from(a[1])
        })
        .sum()
}
