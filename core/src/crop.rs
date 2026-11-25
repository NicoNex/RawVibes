//! Crop and straighten geometry, UI-free.

use serde::{Deserialize, Serialize};

use crate::view::Region;

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum Ratio {
    #[default]
    Free,
    /// The oriented image's own aspect.
    Original,
    Fixed(f32, f32),
}

impl Ratio {
    /// Pixel width / height this ratio holds a crop to (None: free).
    pub fn aspect(self, img_w: u32, img_h: u32) -> Option<f32> {
        match self {
            Ratio::Free => None,
            Ratio::Original => Some(img_w as f32 / img_h as f32),
            Ratio::Fixed(a, b) => Some(a / b),
        }
    }
}

pub const PRESETS: &[(&str, Ratio)] = &[
    ("Free", Ratio::Free),
    ("Original", Ratio::Original),
    ("1:1", Ratio::Fixed(1.0, 1.0)),
    ("5:4", Ratio::Fixed(5.0, 4.0)),
    ("4:5", Ratio::Fixed(4.0, 5.0)),
    ("4:3", Ratio::Fixed(4.0, 3.0)),
    ("3:4", Ratio::Fixed(3.0, 4.0)),
    ("3:2", Ratio::Fixed(3.0, 2.0)),
    ("2:3", Ratio::Fixed(2.0, 3.0)),
    ("16:9", Ratio::Fixed(16.0, 9.0)),
    ("9:16", Ratio::Fixed(9.0, 16.0)),
    ("21:9", Ratio::Fixed(21.0, 9.0)),
    ("2.39:1", Ratio::Fixed(2.39, 1.0)),
    ("XPan 65:24", Ratio::Fixed(65.0, 24.0)),
    ("6:17", Ratio::Fixed(6.0, 17.0)),
];

/// `rect` = x, y, w, h normalised to the oriented image, in the frame
/// rotated by `angle` degrees about the image centre.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Crop {
    pub rect: [f32; 4],
    pub angle: f32,
}

impl Default for Crop {
    fn default() -> Self {
        Self { rect: [0.0, 0.0, 1.0, 1.0], angle: 0.0 }
    }
}

/// Largest centred crop of `ratio` inside the image rotated by `angle`.
pub fn fit_ratio(ratio: Ratio, img_w: u32, img_h: u32, angle: f32) -> Crop {
    let (w, h) = (img_w as f32, img_h as f32);
    let a = ratio.aspect(img_w, img_h).unwrap_or(w / h);
    let (sin, cos) = angle.to_radians().abs().sin_cos();
    // A rect a·ch × ch turned by θ inside W × H: ch·(a·cos + sin) ≤ W, ch·(a·sin + cos) ≤ H.
    let ch = (w / (a * cos + sin)).min(h / (a * sin + cos));
    // Rounding can make the full side a hair over 1, and its origin a hair under 0.
    let (nw, nh) = ((a * ch / w).min(1.0), (ch / h).min(1.0));
    Crop { rect: [(1.0 - nw) / 2.0, (1.0 - nh) / 2.0, nw, nh], angle }
}

/// `c` moved, then if still needed shrunk about its centre (ratio kept),
/// until no corner falls outside the image.
/// Largest crop of `ratio` at `c`'s angle, centred where `c` was (moved
/// only as far as the image needs); Free keeps `c`'s shape.
pub fn refit(c: Crop, ratio: Ratio, img_w: u32, img_h: u32) -> Crop {
    let ratio = match ratio {
        Ratio::Free => Ratio::Fixed(aspect(&c, img_w, img_h), 1.0),
        r => r,
    };
    let fit = fit_ratio(ratio, img_w, img_h, c.angle);
    let [x, y, w, h] = c.rect;
    let [_, _, fw, fh] = fit.rect;
    // Each axis into the frame on its own (`clamp` moves both towards the centre together).
    let (x, y) = ((x + (w - fw) / 2.0).clamp(0.0, 1.0 - fw), (y + (h - fh) / 2.0).clamp(0.0, 1.0 - fh));
    clamp(Crop { rect: [x, y, fw, fh], ..fit }, img_w, img_h)
}

/// The whole straightened frame `c` is cut from.
pub fn frame(c: Crop) -> Crop {
    Crop { rect: [0.0, 0.0, 1.0, 1.0], ..c }
}

pub fn clamp(c: Crop, img_w: u32, img_h: u32) -> Crop {
    let fits = |c: &Crop| fits(c, img_w, img_h);
    let [x, y, w, h] = c.rect;
    let moved = |t: f32| {
        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
        let (cx, cy) = (cx + (0.5 - cx) * t, cy + (0.5 - cy) * t);
        Crop { rect: [cx - w / 2.0, cy - h / 2.0, w, h], ..c }
    };
    let shrunk = |s: f32| {
        let (nw, nh) = (w * s, h * s);
        Crop { rect: [0.5 - nw / 2.0, 0.5 - nh / 2.0, nw, nh], ..c }
    };
    if fits(&c) {
        return c;
    }
    if fits(&moved(1.0)) {
        return moved(search(|t| fits(&moved(t)), false));
    }
    shrunk(search(|s| fits(&shrunk(s)), true))
}

/// Binary search on 0..1 for the boundary of `ok`: the largest passing
/// value when `ok` holds below it (`down`), else the smallest.
fn search(ok: impl Fn(f32) -> bool, down: bool) -> f32 {
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..30 {
        let mid = (lo + hi) / 2.0;
        if ok(mid) == down { lo = mid } else { hi = mid }
    }
    if down { lo } else { hi }
}

/// No corner of `c` outside the image.
fn fits(c: &Crop, img_w: u32, img_h: u32) -> bool {
    corners(&region(c, img_w, img_h), img_w as f32, img_h as f32)
}

/// All corners of `r` within a `w` × `h` image (a hair of slack for rounding).
fn corners(r: &Region, w: f32, h: f32) -> bool {
    [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)].iter().all(|(sx, sy)| {
        let [x, y] = r.point([sx * r.size[0] / 2.0, sy * r.size[1] / 2.0]);
        (-1e-3..=w + 1e-3).contains(&x) && (-1e-3..=h + 1e-3).contains(&y)
    })
}

/// The crop as a region of the oriented image's pixels.
pub fn region(c: &Crop, img_w: u32, img_h: u32) -> Region {
    let (w, h) = (img_w as f32, img_h as f32);
    let [x, y, cw, ch] = c.rect;
    // The frame turns about the image centre; the crop centre is offset from it in that frame.
    let frame = Region { center: [w / 2.0, h / 2.0], size: [cw * w, ch * h], angle: c.angle };
    Region { center: frame.point([(x + cw / 2.0 - 0.5) * w, (y + ch / 2.0 - 0.5) * h]), ..frame }
}

/// What a pointer grabs on the crop rectangle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Handle {
    Move,
    N,
    S,
    E,
    W,
    NE,
    NW,
    SE,
    SW,
}

/// The handle under `p` (normalised frame coordinates), corners first,
/// within `tol` (normalised, per axis) of an edge.
pub fn hit(c: &Crop, p: [f32; 2], tol: [f32; 2]) -> Option<Handle> {
    let [x, y, w, h] = c.rect;
    let (x1, y1) = (x + w, y + h);
    if !(x - tol[0]..=x1 + tol[0]).contains(&p[0]) || !(y - tol[1]..=y1 + tol[1]).contains(&p[1]) {
        return None;
    }
    let near = |v: f32, edge: f32, t: f32| (v - edge).abs() <= t;
    let (l, r) = (near(p[0], x, tol[0]), near(p[0], x1, tol[0]));
    let (t, b) = (near(p[1], y, tol[1]), near(p[1], y1, tol[1]));
    Some(match (l, r, t, b) {
        (true, _, true, _) => Handle::NW,
        (_, true, true, _) => Handle::NE,
        (true, _, _, true) => Handle::SW,
        (_, true, _, true) => Handle::SE,
        (_, _, true, _) => Handle::N,
        (_, _, _, true) => Handle::S,
        (true, ..) => Handle::W,
        (_, true, ..) => Handle::E,
        _ => Handle::Move,
    })
}

/// Smallest crop side, normalised.
const MIN_SIDE: f32 = 0.02;

/// `start` with `h` dragged by `d` (normalised frame units). With
/// `aspect` (pixel w/h), resizing keeps it. Never leaves the image.
pub fn drag(start: Crop, h: Handle, d: [f32; 2], aspect: Option<f32>, img_w: u32, img_h: u32) -> Crop {
    use Handle::*;
    // Pixel aspect of a normalised unit square.
    let px = img_w as f32 / img_h as f32;
    let resized = |t: f32| {
        let [dx, dy] = d.map(|v| v * t);
        let [x, y, w, hh] = start.rect;
        let (mut x0, mut y0, mut x1, mut y1) = (x, y, x + w, y + hh);
        if matches!(h, W | NW | SW) {
            x0 = (x0 + dx).min(x1 - MIN_SIDE);
        }
        if matches!(h, E | NE | SE) {
            x1 = (x1 + dx).max(x0 + MIN_SIDE);
        }
        if matches!(h, N | NW | NE) {
            y0 = (y0 + dy).min(y1 - MIN_SIDE);
        }
        if matches!(h, S | SW | SE) {
            y1 = (y1 + dy).max(y0 + MIN_SIDE);
        }
        if let Some(a) = aspect {
            // Normalised width for a normalised height at this pixel aspect, and back.
            let (wn, hn) = ((y1 - y0) * a / px, (x1 - x0) * px / a);
            let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
            match h {
                N | S => (x0, x1) = (cx - wn / 2.0, cx + wn / 2.0),
                E | W => (y0, y1) = (cy - hn / 2.0, cy + hn / 2.0),
                // Corners follow the larger pointer motion, anchored at the opposite corner.
                _ if dx.abs() * px >= dy.abs() => {
                    if matches!(h, N | NW | NE) {
                        y0 = y1 - hn
                    } else {
                        y1 = y0 + hn
                    }
                }
                _ => {
                    if matches!(h, W | NW | SW) {
                        x0 = x1 - wn
                    } else {
                        x1 = x0 + wn
                    }
                }
            }
        }
        Crop { rect: [x0, y0, x1 - x0, y1 - y0], ..start }
    };
    let fits = |c: &Crop| fits(c, img_w, img_h);
    if h == Move {
        let [x, y, w, hh] = start.rect;
        let at = |m: [f32; 2]| Crop { rect: [x + m[0], y + m[1], w, hh], ..start };
        if !fits(&start) {
            return clamp(at(d), img_w, img_h);
        }
        // Each axis as far as it goes, so the crop slides along an edge instead of
        // being pulled towards the centre; both orders, keeping the one nearer the pointer.
        let slide = |first: usize| {
            let mut m = [0.0; 2];
            for a in [first, 1 - first] {
                let reach = |t: f32| {
                    let mut v = m;
                    v[a] = d[a] * t;
                    fits(&at(v))
                };
                m[a] = d[a] * if reach(1.0) { 1.0 } else { search(reach, true) };
            }
            m
        };
        let miss = |m: [f32; 2]| (m[0] - d[0]).hypot(m[1] - d[1]);
        let (a, b) = (slide(0), slide(1));
        return at(if miss(a) <= miss(b) { a } else { b });
    }
    if fits(&resized(1.0)) {
        return resized(1.0);
    }
    // Already against an edge, rounding can push even the smallest step out: then it stays.
    Some(resized(search(|t| fits(&resized(t)), true))).filter(|c| fits(c)).unwrap_or(start)
}

/// Whether `c` has room to move: one that fills the image cannot, and a drag on it draws a new crop.
pub fn movable(c: &Crop, img_w: u32, img_h: u32) -> bool {
    const STEP: f32 = 0.002;
    [[STEP, 0.0], [-STEP, 0.0], [0.0, STEP], [0.0, -STEP]]
        .into_iter()
        // More than rounding slack: a full crop "moves" by a hair.
        .any(|d| {
            let m = drag(*c, Handle::Move, d, None, img_w, img_h).rect;
            (m[0] - c.rect[0]).abs().max((m[1] - c.rect[1]).abs()) > STEP / 2.0
        })
}

/// A crop drawn by dragging from `from` to `to` (normalised frame coordinates), at
/// `start`'s angle: anchored at `from`, of `aspect` (pixel w/h) when given, shrunk
/// towards `from` to stay inside the image. `None` for a click, or a press off the image.
pub fn draw(start: Crop, from: [f32; 2], to: [f32; 2], aspect: Option<f32>, img_w: u32, img_h: u32) -> Option<Crop> {
    let (mut w, mut h) = ((to[0] - from[0]).abs(), (to[1] - from[1]).abs());
    if w.max(h) < MIN_SIDE {
        return None;
    }
    // Anchored at `from`, growing the way the pointer went.
    let at = |w: f32, h: f32| {
        let x = if to[0] < from[0] { from[0] - w } else { from[0] };
        let y = if to[1] < from[1] { from[1] - h } else { from[1] };
        Crop { rect: [x, y, w, h], angle: start.angle }
    };
    if !fits(&at(MIN_SIDE, MIN_SIDE), img_w, img_h) {
        return None;
    }
    if let Some(a) = aspect {
        // The larger side in pixels sets the other (a normalised unit is img_w/img_h wide per tall).
        let px = img_w as f32 / img_h as f32;
        if w * px >= h * a { h = w * px / a } else { w = h * a / px }
    }
    let (w, h) = (w.max(MIN_SIDE), h.max(MIN_SIDE));
    let ok = |t: f32| fits(&at(w * t, h * t), img_w, img_h);
    let t = if ok(1.0) { 1.0 } else { search(ok, true) };
    Some(at(w * t, h * t))
}

/// `c` at a new straighten `angle`: a crop as large as the image allows
/// stays that large (growing back as the angle returns); any other keeps
/// its framing, shrunk only if a corner would leave the image.
pub fn straighten(c: Crop, angle: f32, img_w: u32, img_h: u32) -> Crop {
    let shape = Ratio::Fixed(aspect(&c, img_w, img_h), 1.0);
    let largest = fit_ratio(shape, img_w, img_h, c.angle);
    if c.rect.iter().zip(largest.rect).all(|(a, b)| (a - b).abs() < 1e-3) {
        fit_ratio(shape, img_w, img_h, angle)
    } else {
        clamp(Crop { angle, ..c }, img_w, img_h)
    }
}

/// Width / height of the crop in pixels.
pub fn aspect(c: &Crop, img_w: u32, img_h: u32) -> f32 {
    c.rect[2] * img_w as f32 / (c.rect[3] * img_h as f32)
}

#[cfg(test)]
mod tests {

    /// Wide ratios fill the width: rounding must not push the origin below 0.
    #[test]
    fn fit_ratio_stays_in_the_frame() {
        for (w, h) in [(3008, 2000), (5776, 4336), (6000, 4000), (4000, 6000)] {
            for (a, b) in [(65.0, 24.0), (16.0, 9.0), (3.0, 2.0), (1.0, 1.0), (4.0, 5.0)] {
                for angle in [-10.0, -0.5, 0.0, 3.0, 45.0] {
                    let c = fit_ratio(Ratio::Fixed(a, b), w, h, angle);
                    assert!(c.rect.iter().all(|v| (0.0..=1.0).contains(v)), "{w}x{h} {a}:{b} {angle} {:?}", c.rect);
                    refit(c, Ratio::Fixed(a, b), w, h); // its `clamp(0, 1 - fw)` panics if fw > 1
                }
            }
        }
    }

    /// A new ratio keeps the framing: centred where the crop was, moved only
    /// as far as the image needs.
    #[test]
    fn refit_keeps_the_framing() {
        let left = Crop { rect: [0.05, 0.1, 0.4, 0.5], angle: 0.0 };
        let c = refit(left, Ratio::Fixed(1.0, 1.0), 3000, 2000);
        let centre = |c: &Crop| [c.rect[0] + c.rect[2] / 2.0, c.rect[1] + c.rect[3] / 2.0];
        // The largest square is 2/3 of the width: as far left as it goes.
        assert!((c.rect[0]).abs() < 1e-4 && (c.rect[2] - 2.0 / 3.0).abs() < 1e-3, "{:?}", c.rect);
        let small = Crop { rect: [0.6, 0.6, 0.2, 0.2], angle: 0.0 };
        let c = refit(small, Ratio::Fixed(3.0, 2.0), 3000, 2000);
        assert!(c.rect[0] + c.rect[2] <= 1.0 + 1e-4 && c.rect[1] + c.rect[3] <= 1.0 + 1e-4, "{:?}", c.rect);
        assert!(centre(&c)[0] > 0.5 - 1e-4, "moved to the middle: {:?}", c.rect);
    }

    use super::*;

    fn inside(r: &Region, w: f32, h: f32) -> bool {
        corners(&Region { size: r.size.map(|v| v - 2e-2), ..*r }, w, h)
    }

    const RECT: Crop = Crop { rect: [0.2, 0.2, 0.6, 0.6], angle: 0.0 };

    #[test]
    fn hit_finds_corners_edges_and_inside() {
        let tol = [0.02, 0.02];
        assert_eq!(hit(&RECT, [0.21, 0.79], tol), Some(Handle::SW));
        assert_eq!(hit(&RECT, [0.5, 0.2], tol), Some(Handle::N));
        assert_eq!(hit(&RECT, [0.5, 0.5], tol), Some(Handle::Move));
        assert_eq!(hit(&RECT, [0.05, 0.5], tol), None);
    }

    #[test]
    fn free_edge_drag_moves_only_that_edge() {
        let c = drag(RECT, Handle::E, [-0.1, 0.3], None, 6000, 4000);
        assert!(c.rect.iter().zip([0.2, 0.2, 0.5, 0.6]).all(|(a, b)| (a - b).abs() < 1e-5), "{c:?}");
    }

    #[test]
    fn corner_drag_keeps_ratio_and_anchors_the_opposite_corner() {
        let start = Crop { rect: [0.2, 0.2, 0.4, 0.4], angle: 0.0 }; // 2400×1600 px = 3:2
        let c = drag(start, Handle::SE, [0.1, 0.0], Some(1.5), 6000, 4000);
        assert!((aspect(&c, 6000, 4000) - 1.5).abs() < 1e-3, "{c:?}");
        assert!((c.rect[0] - 0.2).abs() < 1e-5 && (c.rect[1] - 0.2).abs() < 1e-5, "{c:?}");
        assert!(c.rect[2] > 0.45, "{c:?}");
    }

    /// Pushed against an edge, the crop slides along it: the other axis keeps following the pointer.
    #[test]
    fn move_slides_along_the_edge() {
        let start = Crop { rect: [0.3, 0.1, 0.4, 0.4], angle: 0.0 };
        let c = drag(start, Handle::Move, [0.5, 0.1], None, 6000, 4000);
        assert!((c.rect[0] - 0.6).abs() < 1e-4 && (c.rect[1] - 0.2).abs() < 1e-4, "{c:?}");
        // Turned: still inside, still moved down as far as asked.
        let start = Crop { rect: [0.3, 0.1, 0.3, 0.3], angle: 5.0 };
        let c = drag(start, Handle::Move, [0.0, 0.1], None, 6000, 4000);
        assert!(inside(&region(&c, 6000, 4000), 6000.0, 4000.0) && (c.rect[1] - 0.2).abs() < 1e-4, "{c:?}");
    }

    #[test]
    fn draw_spans_the_drag() {
        let c = draw(Crop::default(), [0.6, 0.5], [0.2, 0.3], None, 6000, 4000).unwrap();
        let want = [0.2, 0.3, 0.4, 0.2];
        assert!(c.rect.iter().zip(want).all(|(a, b)| (a - b).abs() < 1e-5), "{c:?}");
    }

    #[test]
    fn draw_keeps_the_ratio_and_the_image() {
        // XPan 65:24 from near the right edge, dragged left and down.
        let c = draw(Crop::default(), [0.9, 0.2], [0.3, 0.3], Some(65.0 / 24.0), 6000, 4000).unwrap();
        assert!((aspect(&c, 6000, 4000) - 65.0 / 24.0).abs() < 1e-3, "{c:?}");
        assert!((c.rect[0] + c.rect[2] - 0.9).abs() < 1e-5 && (c.rect[1] - 0.2).abs() < 1e-5, "{c:?}");
        // Too big for the image below the anchor: shrunk, still anchored, still inside.
        let c = draw(Crop::default(), [0.1, 0.8], [0.9, 0.9], Some(1.0), 6000, 4000).unwrap();
        assert!(fits(&c, 6000, 4000) && (c.rect[0] - 0.1).abs() < 1e-5 && (c.rect[1] - 0.8).abs() < 1e-5, "{c:?}");
        assert!((aspect(&c, 6000, 4000) - 1.0).abs() < 1e-3, "{c:?}");
    }

    /// Every preset keeps its ratio under every handle, drag direction and angle, and stays inside.
    #[test]
    fn presets_keep_their_ratio_when_resized() {
        use Handle::*;
        let (w, h) = (6000, 4000);
        let moves =
            [[0.3, 0.0], [-0.3, 0.0], [0.0, 0.3], [0.0, -0.3], [0.2, 0.15], [-0.25, 0.1], [0.9, -0.9], [-2.0, 2.0]];
        for (name, ratio) in PRESETS.iter().filter(|(_, r)| *r != Ratio::Free) {
            let a = ratio.aspect(w, h).unwrap();
            for angle in [0.0, 7.0] {
                let start = refit(Crop { rect: [0.3, 0.3, 0.3, 0.3], angle }, *ratio, w, h);
                let start = drag(start, SE, [-0.2, -0.2], Some(a), w, h);
                for handle in [N, S, E, W, NE, NW, SE, SW] {
                    for d in moves {
                        let c = drag(start, handle, d, Some(a), w, h);
                        assert!((aspect(&c, w, h) - a).abs() < a * 2e-3, "{name} {angle}° {handle:?} {d:?}: {c:?}");
                        assert!(fits(&c, w, h), "{name} {angle}° {handle:?} {d:?}: {c:?}");
                    }
                }
                let c = draw(start, [0.5, 0.5], [0.1, 0.9], Some(a), w, h).unwrap();
                assert!((aspect(&c, w, h) - a).abs() < a * 2e-3 && fits(&c, w, h), "{name} {angle}° draw: {c:?}");
            }
        }
    }

    #[test]
    fn a_full_crop_cannot_move() {
        assert!(!movable(&Crop::default(), 6000, 4000));
        assert!(!movable(&fit_ratio(Ratio::Original, 6000, 4000, 8.0), 6000, 4000));
        // XPan across the full width still moves up and down.
        assert!(movable(&fit_ratio(Ratio::Fixed(65.0, 24.0), 6000, 4000, 0.0), 6000, 4000));
    }

    #[test]
    fn a_click_draws_nothing() {
        assert_eq!(draw(Crop::default(), [0.5, 0.5], [0.501, 0.5], None, 6000, 4000), None);
        assert_eq!(draw(Crop::default(), [1.2, 0.5], [0.5, 0.2], None, 6000, 4000), None);
    }

    #[test]
    fn move_keeps_size_and_stays_inside() {
        let c = drag(RECT, Handle::Move, [5.0, -5.0], None, 6000, 4000);
        assert!((c.rect[2] - 0.6).abs() < 1e-5 && (c.rect[3] - 0.6).abs() < 1e-5, "{c:?}");
        assert!(inside(&region(&c, 6000, 4000), 6000.0, 4000.0), "{c:?}");
    }

    #[test]
    fn resize_never_leaves_the_rotated_image() {
        let start = fit_ratio(Ratio::Original, 6000, 4000, 10.0);
        let c = drag(start, Handle::NW, [-1.0, -1.0], None, 6000, 4000);
        assert!(inside(&region(&c, 6000, 4000), 6000.0, 4000.0), "{c:?}");
    }

    #[test]
    fn edges_cannot_cross() {
        let c = drag(RECT, Handle::W, [2.0, 0.0], None, 6000, 4000);
        assert!(c.rect[2] > 0.0, "{c:?}");
    }

    #[test]
    fn straightening_a_full_crop_refits_it() {
        let tilted = straighten(Crop::default(), 10.0, 6000, 4000);
        let back = straighten(tilted, 0.0, 6000, 4000);
        assert!(back.rect.iter().zip([0.0, 0.0, 1.0, 1.0]).all(|(a, b)| (a - b).abs() < 1e-4), "{back:?}");
    }

    #[test]
    fn straightening_a_small_crop_keeps_its_framing() {
        let small = Crop { rect: [0.1, 0.1, 0.3, 0.3], angle: 0.0 };
        let c = straighten(small, 1.0, 6000, 4000);
        assert!(c.rect.iter().zip(small.rect).all(|(a, b)| (a - b).abs() < 1e-5), "{c:?}");
        assert_eq!(c.angle, 1.0);
    }

    #[test]
    fn default_is_full_image() {
        assert_eq!(
            region(&Crop::default(), 6000, 4000),
            Region { center: [3000.0, 2000.0], size: [6000.0, 4000.0], angle: 0.0 }
        );
    }

    #[test]
    fn xpan_ratio_on_landscape() {
        let c = fit_ratio(Ratio::Fixed(65.0, 24.0), 6000, 4000, 0.0);
        assert!((aspect(&c, 6000, 4000) - 65.0 / 24.0).abs() < 1e-3);
        assert!((c.rect[2] - 1.0).abs() < 1e-5, "{c:?}");
    }

    #[test]
    fn portrait_ratio_on_portrait() {
        let c = fit_ratio(Ratio::Fixed(2.0, 3.0), 4000, 6000, 0.0);
        assert!(c.rect.iter().zip([0.0, 0.0, 1.0, 1.0]).all(|(a, b)| (a - b).abs() < 1e-5), "{c:?}");
    }

    #[test]
    fn rotated_crop_stays_inside() {
        for angle in [-45.0, -10.0, 5.0, 30.0, 45.0] {
            let r = region(&fit_ratio(Ratio::Original, 6000, 4000, angle), 6000, 4000);
            assert!(inside(&r, 6000.0, 4000.0), "{angle}: {r:?}");
            assert!(r.size[0] > 100.0, "{angle}: degenerate {r:?}");
        }
    }

    #[test]
    fn clamp_keeps_ratio() {
        let c = clamp(Crop { rect: [0.0, 0.1, 1.0, 0.8], angle: 10.0 }, 6000, 4000);
        assert!((aspect(&c, 6000, 4000) - 6000.0 / 3200.0).abs() < 1e-3, "{c:?}");
        assert!(inside(&region(&c, 6000, 4000), 6000.0, 4000.0), "{c:?}");
    }

    #[test]
    fn clamp_brings_back_an_outside_rect() {
        let c = clamp(Crop { rect: [0.8, 0.8, 0.5, 0.5], angle: 0.0 }, 6000, 4000);
        assert!(inside(&region(&c, 6000, 4000), 6000.0, 4000.0), "{c:?}");
        assert!(c.rect[2] > 0.1, "collapsed: {c:?}");
    }
}
