//! Crop and straighten geometry, UI-free.

use serde::{Deserialize, Serialize};

use crate::view::Region;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Ratio {
    Free,
    /// The oriented image's own aspect.
    Original,
    Fixed(f32, f32),
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
    let a = match ratio {
        Ratio::Free | Ratio::Original => w / h,
        Ratio::Fixed(x, y) => x / y,
    };
    let (sin, cos) = angle.to_radians().abs().sin_cos();
    // A rect a·ch × ch turned by θ inside W × H: ch·(a·cos + sin) ≤ W, ch·(a·sin + cos) ≤ H.
    let ch = (w / (a * cos + sin)).min(h / (a * sin + cos));
    let (nw, nh) = (a * ch / w, ch / h);
    Crop { rect: [(1.0 - nw) / 2.0, (1.0 - nh) / 2.0, nw, nh], angle }
}

/// `c` moved, then if still needed shrunk about its centre (ratio kept),
/// until no corner falls outside the image.
pub fn clamp(c: Crop, img_w: u32, img_h: u32) -> Crop {
    let fits = |c: &Crop| corners(&region(c, img_w, img_h), img_w as f32, img_h as f32);
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

/// All corners of `r` within a `w` × `h` image (a hair of slack for rounding).
fn corners(r: &Region, w: f32, h: f32) -> bool {
    let (sin, cos) = r.angle.to_radians().sin_cos();
    [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)].iter().all(|(sx, sy)| {
        let (dx, dy) = (sx * r.size[0] / 2.0, sy * r.size[1] / 2.0);
        let (x, y) = (r.center[0] + cos * dx - sin * dy, r.center[1] + sin * dx + cos * dy);
        (-1e-3..=w + 1e-3).contains(&x) && (-1e-3..=h + 1e-3).contains(&y)
    })
}

/// The crop as a region of the oriented image's pixels.
pub fn region(c: &Crop, img_w: u32, img_h: u32) -> Region {
    let (w, h) = (img_w as f32, img_h as f32);
    let [x, y, cw, ch] = c.rect;
    // Crop centre relative to the image centre, in the rotated frame, then into the image.
    let (dx, dy) = ((x + cw / 2.0 - 0.5) * w, (y + ch / 2.0 - 0.5) * h);
    let (sin, cos) = c.angle.to_radians().sin_cos();
    Region {
        center: [w / 2.0 + cos * dx - sin * dy, h / 2.0 + sin * dx + cos * dy],
        size: [cw * w, ch * h],
        angle: c.angle,
    }
}

/// Width / height of the crop in pixels.
pub fn aspect(c: &Crop, img_w: u32, img_h: u32) -> f32 {
    c.rect[2] * img_w as f32 / (c.rect[3] * img_h as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inside(r: &Region, w: f32, h: f32) -> bool {
        corners(&Region { size: r.size.map(|v| v - 2e-2), ..*r }, w, h)
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
