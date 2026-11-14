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
        let scale = (r.size[0] / out_w as f32).max(r.size[1] / out_h as f32);
        let level = (scale.max(1.0).log2().floor() as usize).min(self.levels.len() - 1);
        let src = &self.levels[level];
        let k = 1.0 / (1u32 << level) as f32;
        let (sin, cos) = r.angle.to_radians().sin_cos();
        let (sx, sy) = (r.size[0] / out_w as f32, r.size[1] / out_h as f32);
        let pixels = (0..out_h)
            .into_par_iter()
            .flat_map_iter(|v| {
                (0..out_w).map(move |u| {
                    let dx = (u as f32 + 0.5 - out_w as f32 / 2.0) * sx;
                    let dy = (v as f32 + 0.5 - out_h as f32 / 2.0) * sy;
                    let x = r.center[0] + cos * dx - sin * dy;
                    let y = r.center[1] + sin * dx + cos * dy;
                    bilinear(src, x * k, y * k)
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

    #[test]
    fn pyramid_halves_until_one_pixel() {
        let p = Pyramid::new(photo(100, 40, |_, _| [0.5; 4]));
        let sizes: Vec<_> = p.levels.iter().map(|l| (l.width, l.height)).collect();
        assert_eq!(sizes, [(100, 40), (50, 20), (25, 10), (12, 5), (6, 2), (3, 1), (1, 1)]);
    }
}
