//! RawTherapee's auto-matched tone curve (`rtengine/histmatching.cc`,
//! `RawImageSource::getAutoMatchedToneCurve`): the curve that maps the
//! neutral render's luminance histogram onto the camera JPEG's, reduced to a
//! few control points. "As in camera" puts it in the master curve.
//!
//! Ported as is, with one change also made by RapidRAW's port: the CDFs are
//! normalised to 0..1, so the two images need not be resized to the same
//! pixel count.

use crate::grade::{Curve, LUMA};
use crate::photo::Photo;

const N: usize = 256;

struct Cdf {
    cdf: Vec<f64>,
    min_val: i32,
    max_val: i32,
}

/// Normalised cumulative distribution of the 8-bit luminance of `px`.
fn cdf<'a>(px: impl Iterator<Item = &'a [f32; 4]>) -> Cdf {
    let mut hist = vec![0f64; N];
    for p in px {
        let lum = LUMA[0] * p[0] * 255.0 + LUMA[1] * p[1] * 255.0 + LUMA[2] * p[2] * 255.0;
        hist[(lum as i32).clamp(0, 255) as usize] += 1.0;
    }
    let (mut min_val, mut max_val) = (-1, -1);
    let total: f64 = hist.iter().sum::<f64>().max(1.0);
    let mut sum = 0.0;
    for (i, h) in hist.iter_mut().enumerate() {
        if *h > 0.0 {
            if min_val < 0 {
                min_val = i as i32;
            }
            max_val = i as i32;
        }
        sum += *h;
        *h = sum / total;
    }
    Cdf { cdf: hist, min_val, max_val }
}

/// The level of `cdf` whose value is nearest `val`, searching from `j`.
fn find_match(val: f64, cdf: &[f64], mut j: i32) -> i32 {
    let at = |k: i32| cdf[k as usize];
    if at(j) <= val {
        while j < N as i32 {
            if at(j) == val {
                return j;
            } else if at(j) > val {
                return if at(j) - val <= val - at(j - 1) { j } else { j - 1 };
            }
            j += 1;
        }
        255
    } else {
        while j >= 0 {
            if at(j) == val {
                return j;
            } else if at(j) < val {
                return if val - at(j) <= at(j + 1) - val { j } else { j + 1 };
            }
            j -= 1;
        }
        0
    }
}

/// RT's `mappingToCurve`: a few points along `mapping` (-1 = no data), and
/// no concavities in the upper part of the S.
fn mapping_to_curve(mapping: &[i32]) -> Curve {
    let len = mapping.len() as i32;
    let m = |i: i32| mapping[i as usize];
    let mut idx = 15;
    while idx < len && m(idx) < idx {
        idx += 1;
    }
    if idx == len {
        idx = 1;
        while idx < len - 1 && m(idx) < idx {
            idx += 1;
        }
    }
    let coord = |v: i32| v as f64 / 255.0;
    let mut curve: Vec<f64> = vec![0.0, 0.0];
    let doit = |curve: &mut Vec<f64>, start: i32, stop: i32, step: i32, addstart: bool, maxdelta: i32| {
        let maxdelta = if maxdelta == 0 { step * 2 } else { maxdelta };
        let mut prev = start;
        if addstart && m(start) >= 0 {
            curve.extend([coord(start), coord(m(start))]);
        }
        for i in start..stop {
            let v = m(i);
            if v < 0 {
                continue;
            }
            let change = i > 0 && v != m(i - 1);
            let diff = i - prev;
            if (change && (diff - step).abs() <= 1) || diff > maxdelta {
                curve.extend([coord(i), coord(v)]);
                prev = i;
            }
        }
    };
    let mut start = 0;
    while start < idx && (m(start) < 0 || start < idx / 2) {
        start += 1;
    }
    let npoints = 8;
    let mut step = (len / npoints).max(1);
    let end = len;
    if idx <= end / 3 {
        doit(&mut curve, start, idx, idx / 2, true, 0);
        step = (end - idx) / 4;
        doit(&mut curve, idx, end, step, false, step);
    } else {
        doit(&mut curve, start, idx, if idx > step { step } else { idx / 2 }, true, 0);
        let addstart = idx - step > step / 2 && (curve[curve.len() - 2] - coord(idx)).abs() > 0.01;
        doit(&mut curve, idx, end, step, addstart, 0);
    }
    if curve.len() > 2 && 1.0 - curve[curve.len() - 2] <= coord(step) / 3.0 {
        curve.truncate(curve.len() - 2);
    }
    curve.extend([1.0, 1.0]);

    // An S-shaped curve is assumed: drop points that make its upper part concave.
    let getpos = |x: f64, xa: f64, ya: f64, xb: f64, yb: f64| (x - xa) / (xb - xa) * (yb - ya) + ya;
    let mut idx: isize = -1;
    let mut i = curve.len() as isize - 1;
    while i > 0 {
        if curve[i as usize] <= 0.0 {
            idx = i + 1;
            break;
        }
        i -= 2;
    }
    if idx >= 0 && (idx as usize) < curve.len() {
        let mut k = idx as usize;
        while k + 5 < curve.len() {
            let (xa, ya, x, y, xb, yb) =
                (curve[k], curve[k + 1], curve[k + 2], curve[k + 3], curve[k + 4], curve[k + 5]);
            if getpos(x, xa, ya, xb, yb) > y {
                curve.drain(k + 2..k + 4);
            } else {
                k += 2;
            }
        }
    }
    if curve.len() < 4 {
        return crate::grade::identity_curve();
    }
    let mut pts: Curve = curve.chunks_exact(2).map(|p| (p[0] as f32, p[1] as f32)).collect();
    // RT feeds these to its own spline; ours needs strictly increasing x.
    pts.dedup_by(|b, a| b.0 <= a.0);
    pts
}

/// The tone curve that takes `neutral` (the RAW as shot, display-encoded)
/// to the luminance of `camera` (its embedded JPEG); `full_width` is the
/// RAW's. `None` when the JPEG is too small to match (RT's limits).
pub fn auto_tone_curve(neutral: &Photo, camera: &Photo, full_width: u32) -> Option<Curve> {
    let (sw, sh) = (camera.width, camera.height);
    if sw * 33 < full_width || sw * sh < 19200 {
        return None;
    }
    // Crop the neutral render to the JPEG's aspect ratio, centred. The JPEG may
    // not be rotated like the render: compare them in the render's orientation.
    let (sw, sh) = if (sw >= sh) == (neutral.width >= neutral.height) { (sw, sh) } else { (sh, sw) };
    let (mut tw, mut th) = (neutral.width, neutral.height);
    let ratio = |w: u32, h: u32| w.max(h) as f32 / w.min(h) as f32;
    let (mut cx, mut cy) = (0, 0);
    if (ratio(sw, sh) - ratio(tw, th)).abs() > 0.01 {
        if ratio(sw, sh) > ratio(tw, th) {
            let ch = th.saturating_sub((tw as f32 * sh as f32 / sw as f32) as u32);
            cy = ch / 2;
            th -= ch;
        } else {
            let cw = tw.saturating_sub((th as f32 * sw as f32 / sh as f32) as u32);
            cx = cw / 2;
            tw -= cw;
        }
    }
    let target = (cy..cy + th).flat_map(|y| (cx..cx + tw).map(move |x| (y * neutral.width + x) as usize));
    let tcdf = cdf(target.map(|i| &neutral.pixels[i]));
    let scdf = cdf(camera.pixels.iter());
    let mut j = 0;
    let mapping: Vec<i32> = (0..N as i32)
        .map(|i| {
            j = find_match(tcdf.cdf[i as usize], &scdf.cdf, j);
            let inside = (tcdf.min_val..=tcdf.max_val).contains(&i) && (scdf.min_val..=scdf.max_val).contains(&j);
            if inside { j } else { -1 }
        })
        .collect();
    Some(mapping_to_curve(&mapping))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn photo(w: u32, h: u32, f: impl Fn(f32) -> f32) -> Photo {
        let pixels = (0..w * h)
            .map(|i| {
                // A smooth ramp with structure, so every level is populated.
                let t = ((i % w) as f32 / w as f32 * 0.7 + (i / w) as f32 / h as f32 * 0.3).clamp(0.0, 1.0);
                let v = f(t);
                [v, v, v, 1.0]
            })
            .collect();
        Photo { width: w, height: h, pixels }
    }

    /// The camera rendered the same scene with a known S curve: the matched
    /// curve reproduces it within a few levels and is monotone.
    #[test]
    fn matches_a_known_curve() {
        let s_curve = |v: f32| (v - 0.5).signum() * (2.0 * (v - 0.5).abs()).powf(0.7) * 0.5 + 0.5;
        let neutral = photo(600, 400, |t| t);
        let camera = photo(300, 200, s_curve);
        let c = auto_tone_curve(&neutral, &camera, 600).expect("large enough");
        let mut out = vec![0.0; 256];
        crate::grade::sample_curve(&c, &mut out);
        assert!(out.windows(2).all(|w| w[1] >= w[0] - 1e-6), "not monotone: {c:?}");
        for x in [0.15f32, 0.3, 0.5, 0.7, 0.85] {
            let got = out[(x * 255.0) as usize];
            assert!((got - s_curve(x)).abs() < 0.04, "at {x}: {got} vs {} ({c:?})", s_curve(x));
        }
    }

    /// Identical renders give (close to) the identity.
    #[test]
    fn same_render_is_identity() {
        let neutral = photo(600, 400, |t| t);
        let c = auto_tone_curve(&neutral, &photo(300, 200, |t| t), 600).unwrap();
        let mut out = vec![0.0; 256];
        crate::grade::sample_curve(&c, &mut out);
        assert!(out.iter().enumerate().all(|(i, v)| (v - i as f32 / 255.0).abs() < 0.02), "{c:?}");
    }

    /// On a real file: neutral, neutral + matched curve, camera JPEG side by side:
    /// `RAWVALLEY_RAW=x.RW2 RAWVALLEY_OUT=dir cargo test --release -p rawvalley-core sample_match -- --ignored`.
    #[test]
    #[ignore]
    fn sample_match() {
        let (Ok(raw), Ok(out)) = (std::env::var("RAWVALLEY_RAW"), std::env::var("RAWVALLEY_OUT")) else { return };
        let path = std::path::PathBuf::from(raw);
        let d = crate::photo::Decoded::load(&path).unwrap();
        let (w, h) = d.size();
        let whole =
            crate::view::Region { center: [w as f32 / 2.0, h as f32 / 2.0], size: [w as f32, h as f32], angle: 0.0 };
        let neutral = d.encode_fit(&whole, 1024, crate::profiles::Profile::Standard, &Default::default());
        let jpeg = crate::photo::embedded_preview(&path).unwrap();
        let c = auto_tone_curve(&neutral, &jpeg, w).unwrap();
        eprintln!("curve {c:?}");
        let mut lut = vec![0.0; 1024];
        crate::grade::sample_curve(&c, &mut lut);
        let at = |v: f32| lut[(v.clamp(0.0, 1.0) * 1023.0) as usize];
        let (nw, nh) = (neutral.width, neutral.height);
        let mut img = image::RgbImage::new(nw * 3, nh);
        for (i, p) in neutral.pixels.iter().enumerate() {
            let (x, y) = (i as u32 % nw, i as u32 / nw);
            let u = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            img.put_pixel(x, y, image::Rgb([u(p[0]), u(p[1]), u(p[2])]));
            img.put_pixel(nw + x, y, image::Rgb([u(at(p[0])), u(at(p[1])), u(at(p[2]))]));
            let (jx, jy) = (x * jpeg.width / nw, y * jpeg.height / nh);
            let q = jpeg.pixels[(jy * jpeg.width + jx) as usize];
            img.put_pixel(2 * nw + x, y, image::Rgb([u(q[0]), u(q[1]), u(q[2])]));
        }
        img.save(format!("{out}/match.png")).unwrap();
    }

    /// A landscape JPEG of a portrait render (the camera did not rotate it)
    /// with another aspect ratio: matched, no overflow.
    #[test]
    fn unrotated_jpeg_of_another_ratio() {
        let neutral = photo(400, 600, |t| t);
        let c = auto_tone_curve(&neutral, &photo(320, 180, |t| t), 600).unwrap();
        assert!(c.len() >= 2);
    }

    #[test]
    fn tiny_thumbnails_are_skipped() {
        assert!(auto_tone_curve(&photo(600, 400, |t| t), &photo(128, 96, |t| t), 6000).is_none());
    }
}
