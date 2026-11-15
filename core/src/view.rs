//! Full-resolution photos and the views cut from them: a mip pyramid, and
//! resampling of any rotated rectangle of it to an output size (zoom, crop,
//! straighten).

use rayon::prelude::*;

use crate::photo::Photo;

/// A rectangle of the oriented image, in its pixels (pixel `i` spans
/// `i..i + 1`), rotated by `angle` degrees about `center`. Output pixel
/// offsets `d` from the output centre come from source point
/// `center + R(angle)·d`, y pointing down.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Region {
    pub center: [f32; 2],
    pub size: [f32; 2],
    pub angle: f32,
}

impl Region {
    /// The source point at offset `d` from the centre, in the region's rotated frame.
    pub fn point(&self, d: [f32; 2]) -> [f32; 2] {
        let (sin, cos) = self.angle.to_radians().sin_cos();
        let r = rotate(d, sin, cos);
        [self.center[0] + r[0], self.center[1] + r[1]]
    }

    /// This region's rectangle (x0, y0, x1, y1) in `outer`'s 0..1 coordinates;
    /// both turned by the same angle (a view of a crop).
    pub fn rect_in(&self, outer: &Region) -> [f32; 4] {
        let (sin, cos) = outer.angle.to_radians().sin_cos();
        let d = [self.center[0] - outer.center[0], self.center[1] - outer.center[1]];
        let local = rotate(d, -sin, cos);
        let at = |i: usize, side: f32| 0.5 + (local[i] + side * self.size[i] / 2.0) / outer.size[i];
        [at(0, -1.0), at(1, -1.0), at(0, 1.0), at(1, 1.0)]
    }
}

/// `d` turned by the angle whose sine and cosine are given (y down).
fn rotate(d: [f32; 2], sin: f32, cos: f32) -> [f32; 2] {
    [cos * d[0] - sin * d[1], sin * d[0] + cos * d[1]]
}

/// A zoomed view of a crop: `scale` image pixels per device pixel, and
/// the view's centre in the crop (0..1 on each axis, in its rotated frame).
/// Canvas sizes and points are in device pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Zoom {
    pub scale: f32,
    pub center: [f32; 2],
}

/// Closest zoom: 800%.
pub const MIN_SCALE: f32 = 1.0 / 8.0;

impl Zoom {
    /// The scale at which the whole crop fits the canvas.
    pub fn fit_scale(crop: &Region, canvas: [f32; 2]) -> f32 {
        (crop.size[0] / canvas[0]).max(crop.size[1] / canvas[1])
    }

    /// Visible half-extent in crop units (0.5 = the whole axis).
    fn half(&self, crop: &Region, canvas: [f32; 2]) -> [f32; 2] {
        [0, 1].map(|i| (canvas[i] * self.scale / crop.size[i] / 2.0).min(0.5))
    }

    fn clamped(mut self, crop: &Region, canvas: [f32; 2]) -> Self {
        let h = self.half(crop, canvas);
        self.center = [0, 1].map(|i| self.center[i].clamp(h[i], 1.0 - h[i]));
        self
    }

    /// The visible part of the crop, and its size on screen.
    pub fn view(&self, crop: &Region, canvas: [f32; 2]) -> (Region, u32, u32) {
        let z = self.clamped(crop, canvas);
        let h = z.half(crop, canvas);
        let out = [0, 1].map(|i| ((2.0 * h[i] * crop.size[i] / self.scale).round() as u32).max(1));
        let region = Region {
            center: crop.point([0, 1].map(|i| (z.center[i] - 0.5) * crop.size[i])),
            size: [out[0] as f32 * self.scale, out[1] as f32 * self.scale],
            angle: crop.angle,
        };
        (region, out[0], out[1])
    }

    /// Zoom from `from` (None = fit) to `scale`, keeping the crop point under
    /// canvas point `at` in place. None when `scale` shows the whole crop.
    pub fn zoom_at(from: Option<Zoom>, scale: f32, at: [f32; 2], crop: &Region, canvas: [f32; 2]) -> Option<Zoom> {
        let fit = Self::fit_scale(crop, canvas);
        if scale >= fit {
            return None;
        }
        let scale = scale.max(MIN_SCALE);
        let from = from.unwrap_or(Zoom { scale: fit, center: [0.5, 0.5] });
        // The view is centred in the canvas, so a canvas offset from the
        // middle is that offset × scale in the crop.
        let offset = [0, 1].map(|i| (at[i] - canvas[i] / 2.0) / crop.size[i]);
        let point = [0, 1].map(|i| from.center[i] + offset[i] * from.scale);
        let center = [0, 1].map(|i| point[i] - offset[i] * scale);
        Some(Zoom { scale, center }.clamped(crop, canvas))
    }

    /// Moves the view by a drag of `d` canvas pixels, kept inside the crop.
    pub fn pan(self, d: [f32; 2], crop: &Region, canvas: [f32; 2]) -> Self {
        let center = [0, 1].map(|i| self.center[i] - d[i] * self.scale / crop.size[i]);
        Zoom { center, ..self }.clamped(crop, canvas)
    }
}

/// The photo at full resolution and halved copies down to 1×1.
#[derive(Debug)]
pub struct Pyramid {
    levels: Vec<Photo>,
}

impl Pyramid {
    pub fn new(full: Photo) -> Self {
        let mut levels = vec![full];
        while let Some(l) = levels.last().filter(|l| l.width > 1 || l.height > 1) {
            levels.push(halve(l));
        }
        Self { levels }
    }

    pub fn full(&self) -> &Photo {
        &self.levels[0]
    }

    /// `r` resampled to `out_w` × `out_h`, bilinear from the finest level
    /// that is no more than 2× denser than the output (no aliasing).
    pub fn resample(&self, r: &Region, out_w: u32, out_h: u32) -> Photo {
        let (sx, sy) = (r.size[0] / out_w as f32, r.size[1] / out_h as f32);
        let scale = sx.max(sy);
        let level = (scale.max(1.0).log2().floor() as usize).min(self.levels.len() - 1);
        let src = &self.levels[level];
        let k = 1.0 / (1u32 << level) as f32;
        let (sin, cos) = r.angle.to_radians().sin_cos();
        let pixels = (0..out_h)
            .into_par_iter()
            .flat_map_iter(|v| {
                (0..out_w).map(move |u| {
                    let dx = (u as f32 + 0.5 - out_w as f32 / 2.0) * sx;
                    let dy = (v as f32 + 0.5 - out_h as f32 / 2.0) * sy;
                    let [x, y] = rotate([dx, dy], sin, cos);
                    bilinear(src, (r.center[0] + x) * k, (r.center[1] + y) * k)
                })
            })
            .collect();
        Photo { width: out_w, height: out_h, pixels }
    }
}

/// 2×2 box average; an odd last row/column is dropped (a 1-pixel side stays 1).
fn halve(p: &Photo) -> Photo {
    let (w, h) = ((p.width / 2).max(1), (p.height / 2).max(1));
    let at = |x: u32, y: u32| p.pixels[(y.min(p.height - 1) * p.width + x.min(p.width - 1)) as usize];
    let pixels = (0..h)
        .into_par_iter()
        .flat_map_iter(|y| {
            (0..w).map(move |x| {
                let q = [at(2 * x, 2 * y), at(2 * x + 1, 2 * y), at(2 * x, 2 * y + 1), at(2 * x + 1, 2 * y + 1)];
                std::array::from_fn(|c| q.iter().map(|p| p[c]).sum::<f32>() * 0.25)
            })
        })
        .collect();
    Photo { width: w, height: h, pixels }
}

/// Bilinear sample at continuous coordinates (pixel centres at i + 0.5), edges clamped.
fn bilinear(p: &Photo, x: f32, y: f32) -> [f32; 4] {
    let (x, y) = ((x - 0.5).clamp(0.0, (p.width - 1) as f32), (y - 0.5).clamp(0.0, (p.height - 1) as f32));
    let (x0, y0) = (x.floor() as u32, y.floor() as u32);
    let (x1, y1) = ((x0 + 1).min(p.width - 1), (y0 + 1).min(p.height - 1));
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let at = |x: u32, y: u32| p.pixels[(y * p.width + x) as usize];
    let (a, b, c, d) = (at(x0, y0), at(x1, y0), at(x0, y1), at(x1, y1));
    std::array::from_fn(|i| {
        let top = a[i] + (b[i] - a[i]) * fx;
        let bottom = c[i] + (d[i] - c[i]) * fx;
        top + (bottom - top) * fy
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn photo(width: u32, height: u32, f: impl Fn(u32, u32) -> [f32; 4]) -> Photo {
        let pixels = (0..height).flat_map(|y| (0..width).map(move |x| (x, y))).map(|(x, y)| f(x, y)).collect();
        Photo { width, height, pixels }
    }

    fn mean(p: &Photo) -> [f32; 3] {
        let n = p.pixels.len() as f32;
        [0, 1, 2].map(|c| p.pixels.iter().map(|px| px[c]).sum::<f32>() / n)
    }

    #[test]
    fn identity_region_returns_source() {
        let src = photo(4, 3, |x, y| [x as f32 / 3.0, y as f32 / 2.0, (x + y) as f32 / 5.0, 1.0]);
        let out =
            Pyramid::new(src.clone()).resample(&Region { center: [2.0, 1.5], size: [4.0, 3.0], angle: 0.0 }, 4, 3);
        assert_eq!((out.width, out.height), (4, 3));
        for (a, b) in out.pixels.iter().zip(&src.pixels) {
            assert!((0..4).all(|c| (a[c] - b[c]).abs() < 1e-5), "{a:?} != {b:?}");
        }
    }

    #[test]
    fn downsample_preserves_mean() {
        // Deterministic pseudo-random noise.
        let src = photo(64, 64, |x, y| {
            let h = (x.wrapping_mul(73_856_093) ^ y.wrapping_mul(19_349_663)) % 1000;
            [h as f32 / 1000.0, (h % 7) as f32 / 7.0, (h % 13) as f32 / 13.0, 1.0]
        });
        let expected = mean(&src);
        let out = Pyramid::new(src).resample(&Region { center: [32.0, 32.0], size: [64.0, 64.0], angle: 0.0 }, 8, 8);
        let got = mean(&out);
        assert!((0..3).all(|c| (got[c] - expected[c]).abs() < 1e-3), "{got:?} != {expected:?}");
    }

    #[test]
    fn rotation_90_maps_corners() {
        let src = photo(4, 4, |x, y| if (x, y) == (0, 0) { [1.0; 4] } else { [0.0, 0.0, 0.0, 1.0] });
        let out = Pyramid::new(src).resample(&Region { center: [2.0, 2.0], size: [4.0, 4.0], angle: 90.0 }, 4, 4);
        let bright: Vec<_> = (0..16).filter(|&i| out.pixels[i][0] > 0.5).map(|i| (i % 4, i / 4)).collect();
        assert_eq!(bright, [(0, 3)]);
    }

    const CROP: Region = Region { center: [3000.0, 2000.0], size: [6000.0, 4000.0], angle: 0.0 };

    /// A zoomed view's rectangle in its crop's 0..1 coordinates, also turned.
    #[test]
    fn rect_in_maps_a_view_into_the_crop() {
        for angle in [0.0, 12.0] {
            let crop = Region { angle, ..CROP };
            let whole = crop.rect_in(&crop);
            assert!(whole.iter().zip([0.0, 0.0, 1.0, 1.0]).all(|(a, b)| (a - b).abs() < 1e-5), "{whole:?}");
            let view = Region { center: crop.point([1500.0, 0.0]), size: [600.0, 400.0], angle };
            let r = view.rect_in(&crop);
            let want = [0.7, 0.45, 0.8, 0.55];
            assert!(r.iter().zip(want).all(|(a, b)| (a - b).abs() < 1e-4), "{angle}: {r:?}");
        }
    }
    const CANVAS: [f32; 2] = [1500.0, 1000.0];

    #[test]
    fn fit_scale_fits_the_crop() {
        assert_eq!(Zoom::fit_scale(&CROP, CANVAS), 4.0);
    }

    #[test]
    fn centred_view_at_100_percent() {
        let (r, w, h) = Zoom { scale: 1.0, center: [0.5, 0.5] }.view(&CROP, CANVAS);
        assert_eq!((r, w, h), (Region { center: [3000.0, 2000.0], size: [1500.0, 1000.0], angle: 0.0 }, 1500, 1000));
    }

    #[test]
    fn zoom_keeps_the_point_under_the_cursor() {
        // At fit the canvas's top-left corner shows the image's; zooming there keeps it.
        let z = Zoom::zoom_at(None, 1.0, [0.0, 0.0], &CROP, CANVAS).unwrap();
        let (r, ..) = z.view(&CROP, CANVAS);
        assert_eq!((r.center, r.size), ([750.0, 500.0], [1500.0, 1000.0]));
    }

    #[test]
    fn zooming_past_the_closest_zoom_does_not_move_the_view() {
        let max = Zoom { scale: MIN_SCALE, center: [0.5, 0.5] };
        assert_eq!(Zoom::zoom_at(Some(max), MIN_SCALE / 2.0, [10.0, 10.0], &CROP, CANVAS), Some(max));
    }

    #[test]
    fn zooming_out_past_fit_is_fit() {
        assert_eq!(Zoom::zoom_at(None, 5.0, [10.0, 10.0], &CROP, CANVAS), None);
    }

    #[test]
    fn pan_stays_inside_the_crop() {
        let z = Zoom { scale: 1.0, center: [0.5, 0.5] }.pan([1e6, -1e6], &CROP, CANVAS);
        let (r, ..) = z.view(&CROP, CANVAS);
        // Dragging right/up shows the left/bottom edge.
        assert_eq!(r.center, [750.0, 3500.0]);
    }

    #[test]
    fn small_crop_is_shown_whole_at_100_percent() {
        let small = Region { center: [400.0, 300.0], size: [800.0, 600.0], angle: 0.0 };
        let (r, w, h) = Zoom { scale: 1.0, center: [0.2, 0.9] }.view(&small, CANVAS);
        assert_eq!((r.center, w, h), ([400.0, 300.0], 800, 600));
    }

    #[test]
    fn view_follows_a_rotated_crop() {
        let rotated = Region { angle: 90.0, ..CROP };
        let (r, ..) = Zoom { scale: 1.0, center: [0.0, 0.5] }.pan([0.0, 0.0], &rotated, CANVAS).view(&rotated, CANVAS);
        // The crop's left edge, turned 90°, lies above its centre.
        assert_eq!(r.center.map(f32::round), [3000.0, 2000.0 - 3000.0 + 750.0]);
        assert_eq!(r.angle, 90.0);
    }

    #[test]
    fn pyramid_halves_until_one_pixel() {
        let p = Pyramid::new(photo(100, 40, |_, _| [0.5; 4]));
        let sizes: Vec<_> = p.levels.iter().map(|l| (l.width, l.height)).collect();
        assert_eq!(sizes, [(100, 40), (50, 20), (25, 10), (12, 5), (6, 2), (3, 1), (1, 1)]);
    }
}
