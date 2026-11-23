//! Bracket merge in the RAW domain: several exposures of one scene become one
//! float mosaic with their combined dynamic range, written as a DNG that opens
//! like any RAW (highlight reconstruction, demosaic, NR and HDR export all run
//! on it unchanged). As Lightroom's and darktable's HDR merge, before demosaic.
//!
//! - Exposure ratios come from the data (a median over well-exposed blocks),
//!   not from EXIF, whose shutter and aperture values are rounded.
//! - Each frame is aligned to the reference (the middle exposure) by a global
//!   translation in whole CFA periods, coarse to fine on a block pyramid.
//! - Each photosite is the inverse-variance weighted mean of the frames'
//!   radiance estimates (Granados et al., "Optimal HDR reconstruction with
//!   linear digital cameras", CVPR 2010), with the Poisson–Gaussian noise model
//!   estimated on the reference (`noise.rs`); clipped photosites weigh nothing.
//! - Ghosts: a frame is left out of a block where it differs from the
//!   reference by more than its noise allows (the robust merge of
//!   Granados et al. 2013 and HDR+, per CFA block).

use std::path::Path;

use rayon::prelude::*;

use crate::noise::{self, NoiseModel};

/// Values at or above this are clipped (the mosaic is scaled to white = 1).
const CLIP: f32 = 0.98;
/// Blocks darker than this (of white) tell nothing about the exposure ratio.
const DARK: f32 = 0.02;
/// Ghost test: noise standard deviations allowed, plus a relative tolerance
/// for residual misalignment and ratio error.
const GHOST_SIGMAS: f32 = 4.0;
const GHOST_REL: f32 = 0.1;
/// Largest alignment shift searched, in CFA periods at full resolution.
const MAX_SHIFT: i32 = 128;

/// One bracketed frame: a scaled mosaic (black 0, white 1).
pub struct Frame {
    pub pixels: Vec<f32>,
}

/// The merge of frames of `w` × `h` with a `period`-pixel CFA.
pub struct Merged {
    /// In the darkest frame's units: ≤ 1 except where every frame clipped.
    pub pixels: Vec<f32>,
    /// Each input frame's exposure relative to the darkest (≥ 1), and its shift (pixels) onto the reference.
    pub exposure: Vec<f32>,
    pub shift: Vec<[i32; 2]>,
    /// The reference (middle exposure) frame.
    pub reference: usize,
}

/// Mean of each `period` × `period` block, and whether any of its photosites clipped.
struct Blocks {
    mean: Vec<f32>,
    clipped: Vec<bool>,
    w: usize,
    h: usize,
}

impl Blocks {
    fn new(px: &[f32], w: usize, h: usize, p: usize) -> Self {
        let (bw, bh) = (w / p, h / p);
        let (mean, clipped) = (0..bw * bh)
            .into_par_iter()
            .map(|i| {
                let (x0, y0) = (i % bw * p, i / bw * p);
                let (mut s, mut clip) = (0.0, false);
                for y in y0..y0 + p {
                    for &v in &px[y * w + x0..y * w + x0 + p] {
                        s += v;
                        clip |= v >= CLIP;
                    }
                }
                (s / (p * p) as f32, clip)
            })
            .unzip();
        Self { mean, clipped, w: bw, h: bh }
    }

    /// Block (x, y) shifted by `d` blocks, if inside.
    fn at(&self, x: usize, y: usize, d: [i32; 2]) -> Option<usize> {
        let (sx, sy) = (x as i32 + d[0], y as i32 + d[1]);
        (sx >= 0 && sy >= 0 && (sx as usize) < self.w && (sy as usize) < self.h)
            .then(|| sy as usize * self.w + sx as usize)
    }
}

/// Exposure of `b` over `a` (frame b's value per unit of frame a's), `b` shifted by `d` blocks,
/// from the blocks well exposed in both: Σb / Σa over those within 1.5× of the median
/// ratio. The median drops what moved or is misaligned; the sums avoid the 1/a noise bias
/// a median of ratios has. `None` with too few blocks.
fn ratio(a: &Blocks, b: &Blocks, d: [i32; 2]) -> Option<f32> {
    let ok = |bl: &Blocks, k: usize| !bl.clipped[k] && bl.mean[k] > DARK && bl.mean[k] < 0.8;
    let pairs: Vec<(f32, f32)> = (0..a.h * a.w)
        .filter_map(|i| {
            let j = b.at(i % a.w, i / a.w, d)?;
            (ok(a, i) && ok(b, j)).then(|| (a.mean[i], b.mean[j]))
        })
        .collect();
    if pairs.len() < 64 {
        return None;
    }
    let mut r: Vec<f32> = pairs.iter().map(|(x, y)| y / x).collect();
    let mid = r.len() / 2;
    let median = *r.select_nth_unstable_by(mid, f32::total_cmp).1;
    let (sa, sb) = pairs
        .iter()
        .filter(|(x, y)| (y / x / median - 1.0).abs() < 0.5)
        .fold((0.0f64, 0.0f64), |(sa, sb), (x, y)| (sa + *x as f64, sb + *y as f64));
    Some((sb / sa) as f32)
}

/// The shift (blocks) that best lays `b` on `a`, given `b`'s exposure `e` relative to `a`:
/// mean absolute difference of the square roots (variance-stabilised) of both, over the
/// blocks neither clips (NaN marks the others), searched coarse to fine on a 2× pyramid.
fn align(a: &Blocks, b: &Blocks, e: f32) -> [i32; 2] {
    // In a's units; what either frame clips tells nothing.
    let cap = CLIP.min(CLIP / e) * 0.95;
    let level0 = |bl: &Blocks, k: f32| -> Vec<f32> {
        bl.mean
            .iter()
            .zip(&bl.clipped)
            .map(|(&v, &c)| if c || v * k >= cap { f32::NAN } else { (v * k).max(0.0).sqrt() })
            .collect()
    };
    let mut pyr = vec![((level0(a, 1.0), level0(b, 1.0 / e)), a.w, a.h)];
    while pyr.last().is_some_and(|(_, w, h)| (*w).min(*h) >= 128) {
        let ((pa, pb), w, h) = pyr.last().expect("non-empty");
        let (nw, nh) = (w / 2, h / 2);
        // A NaN in a 2×2 group makes the coarse block NaN too.
        let half = |p: &Vec<f32>| {
            (0..nw * nh)
                .map(|i| {
                    let (x, y) = (i % nw * 2, i / nw * 2);
                    (p[y * w + x] + p[y * w + x + 1] + p[(y + 1) * w + x] + p[(y + 1) * w + x + 1]) / 4.0
                })
                .collect::<Vec<_>>()
        };
        pyr.push(((half(pa), half(pb)), nw, nh));
    }
    // One window of `a` for every candidate of a level (its margin the largest shift tried):
    // a mean over varying overlaps would favour shifts onto quieter areas.
    let cost = |pa: &[f32], pb: &[f32], w: usize, m: usize, ys: std::ops::Range<usize>, d: [i32; 2]| {
        let (s, n) = ys
            .into_par_iter()
            .map(|y| {
                let yb = (y as isize + d[1] as isize) as usize;
                (m..w - m)
                    .map(|x| (pa[y * w + x] - pb[yb * w + (x as isize + d[0] as isize) as usize]).abs())
                    .filter(|v| !v.is_nan())
                    .fold((0.0f32, 0usize), |(s, n), v| (s + v, n + 1))
            })
            .reduce(|| (0.0, 0), |x, y| (x.0 + y.0, x.1 + y.1));
        // Too little in common to judge: never the best.
        if n < 64 { f32::INFINITY } else { s / n as f32 }
    };
    let top = pyr.len() - 1;
    let mut d = [0i32, 0];
    for (level, ((pa, pb), w, h)) in pyr.iter().enumerate().rev() {
        let base = if level == top { [0, 0] } else { [d[0] * 2, d[1] * 2] };
        let r = if level == top { (MAX_SHIFT >> top).max(2).min((*w.min(h) as i32) / 4) } else { 1 };
        let m = (base[0].abs().max(base[1].abs()) + r) as usize;
        if 2 * m + 8 > (*w).min(*h) {
            break;
        }
        let candidates = (-r..=r).flat_map(|dy| (-r..=r).map(move |dx| [base[0] + dx, base[1] + dy]));
        d = candidates
            .map(|c| (cost(pa, pb, *w, m, m..h - m, c), c))
            .min_by(|x, y| x.0.total_cmp(&y.0))
            .map_or(base, |(_, c)| c);
    }
    d
}

/// Merges `frames` (same size and CFA) of `w` × `h` with a `period`-pixel CFA.
pub fn merge(frames: &[Frame], w: usize, h: usize, period: usize) -> Result<Merged, String> {
    if frames.len() < 2 {
        return Err("Merging to HDR needs at least two photos".into());
    }
    let blocks: Vec<Blocks> = frames.par_iter().map(|f| Blocks::new(&f.pixels, w, h, period)).collect();
    // Brightness order from the overall level of each frame's unclipped blocks: the middle one is the reference.
    let level = |b: &Blocks| b.mean.iter().zip(&b.clipped).map(|(&m, &c)| if c { 1.0 } else { m }).sum::<f32>();
    let mut order: Vec<usize> = (0..frames.len()).collect();
    order.sort_by(|&i, &j| level(&blocks[i]).total_cmp(&level(&blocks[j])));
    let reference = order[order.len() / 2];
    let r = &blocks[reference];
    // Exposure and shift relative to the reference, chained through neighbours in exposure
    // order (each pair overlaps well even across a wide bracket): unaligned estimate,
    // alignment, then the estimate again on the aligned blocks.
    let mut rel = vec![1.0f32; frames.len()];
    let mut shift = vec![[0i32; 2]; frames.len()];
    let mid = order.len() / 2;
    let chain = (mid + 1..order.len())
        .map(|k| (order[k - 1], order[k]))
        .chain((0..mid).rev().map(|k| (order[k + 1], order[k])));
    for (prev, cur) in chain {
        let (a, b) = (&blocks[prev], &blocks[cur]);
        let e0 = ratio(a, b, [0, 0]).ok_or("The photos have too little in common to merge")?;
        let d = align(a, b, e0);
        rel[cur] = rel[prev] * ratio(a, b, d).unwrap_or(e0);
        shift[cur] = [shift[prev][0] + d[0], shift[prev][1] + d[1]];
    }
    let darkest = rel.iter().copied().fold(f32::INFINITY, f32::min);
    let exposure: Vec<f32> = rel.iter().map(|&e| e / darkest).collect();

    // Noise of the reference's photosites, σ² = a·v + b, on its brightest CFA position's
    // plane (green): one colour, full resolution.
    let plane = |ox: usize, oy: usize| -> Vec<f32> {
        (0..r.h * r.w).map(|k| frames[reference].pixels[(k / r.w * period + oy) * w + k % r.w * period + ox]).collect()
    };
    let (gx, gy) = (0..period * period)
        .map(|k| (k % period, k / period))
        .max_by(|a, b| plane(a.0, a.1).iter().sum::<f32>().total_cmp(&plane(b.0, b.1).iter().sum()))
        .expect("period ≥ 1");
    let NoiseModel { a, b } = noise::estimate(&plane(gx, gy).iter().map(|&v| [v; 3]).collect::<Vec<_>>(), r.w, r.h);
    // shortcut: no flat area to measure: a high-ISO-like model, so ghost rejection errs towards keeping frames.
    let (a, b) = if a > 0.0 { (a, b.max(1e-8)) } else { (2e-3, 1e-5) };
    // A block mean averages p² photosites.
    let n = (period * period) as f32;
    let var = |v: f32, e: f32| (a * v + b) / (e * e);

    // Per block (`nf` flags each), the frames that agree with the reference (or, where
    // it clips, with the brightest unclipped frame darker than it).
    let nf = frames.len();
    let agree: Vec<bool> = (0..r.h * r.w)
        .into_par_iter()
        .flat_map_iter(|k| {
            let (blocks, shift, order, exposure) = (&blocks, &shift, &order, &exposure);
            let (x, y) = (k % r.w, k / r.w);
            let at = move |i: usize| blocks[i].at(x, y, shift[i]);
            let anchor =
                order[..=order.len() / 2].iter().rev().copied().find(|&i| at(i).is_some_and(|j| !blocks[i].clipped[j]));
            (0..nf).map(move |i| {
                let (Some(c), Some(j)) = (anchor, at(i)) else { return true };
                let Some(jc) = at(c) else { return true };
                if i == c || blocks[i].clipped[j] {
                    return true;
                }
                let (vi, vc) = (blocks[i].mean[j], blocks[c].mean[jc]);
                let (xi, xc) = (vi / exposure[i], vc / exposure[c]);
                let sigma = ((var(vi, exposure[i]) + var(vc, exposure[c])) / n).sqrt();
                (xi - xc).abs() <= GHOST_SIGMAS * sigma + GHOST_REL * xc
            })
        })
        .collect();

    let p = period as i32;
    let dark = order[0];
    let pixels: Vec<f32> = (0..h)
        .into_par_iter()
        .flat_map_iter(|y| {
            let (agree, frames, shift, exposure) = (&agree, frames, &shift, &exposure);
            (0..w).map(move |x| {
                let (bx, by) = ((x / period).min(r.w.saturating_sub(1)), (y / period).min(r.h.saturating_sub(1)));
                let ok = &agree[(by * r.w + bx) * nf..][..nf];
                let (mut sum, mut wsum) = (0.0f32, 0.0f32);
                for (i, f) in frames.iter().enumerate() {
                    let (sx, sy) = (x as i32 + shift[i][0] * p, y as i32 + shift[i][1] * p);
                    if !ok[i] || sx < 0 || sy < 0 || sx >= w as i32 || sy >= h as i32 {
                        continue;
                    }
                    let v = f.pixels[sy as usize * w + sx as usize];
                    if v >= CLIP {
                        continue;
                    }
                    let wi = 1.0 / var(v.max(0.0), exposure[i]);
                    sum += wi * v / exposure[i];
                    wsum += wi;
                }
                if wsum > 0.0 {
                    sum / wsum
                } else {
                    // Clipped everywhere: the darkest frame's value, clipped, for highlight reconstruction.
                    let (sx, sy) = (x as i32 + shift[dark][0] * p, y as i32 + shift[dark][1] * p);
                    let inside = sx >= 0 && sy >= 0 && sx < w as i32 && sy < h as i32;
                    if inside { frames[dark].pixels[sy as usize * w + sx as usize] } else { 1.0 }
                }
            })
        })
        .collect();
    let shift = shift.iter().map(|d| [d[0] * p, d[1] * p]).collect();
    Ok(Merged { pixels, exposure, shift, reference })
}

/// Merges the bracketed RAWs at `paths` into a float DNG at `out`, opening at the
/// reference's exposure (DNG BaselineExposure). Bayer and X-Trans mosaics; the
/// result's pixels are left empty (they went to the file).
pub fn merge_files(paths: &[&Path], out: &Path) -> Result<Merged, String> {
    use rawler::RawImageData;
    use rawler::decoders::RawDecodeParams;
    use rawler::dng::writer::DngWriter;
    use rawler::dng::{CropMode, DNG_VERSION_V1_4, DngCompression, DngPhotometricConversion};
    use rawler::formats::tiff::SRational;
    use rawler::rawimage::RawPhotometricInterpretation;
    use rawler::tags::{DngTag, ExifTag};

    let mut decoded: Vec<(rawler::RawImage, u16)> = paths
        .par_iter()
        .map(|p| {
            let (mut raw, orientation) = crate::raw::decode(p)?;
            if raw.cpp != 1 || !matches!(raw.photometric, RawPhotometricInterpretation::Cfa(_)) {
                return Err(format!("{}: merging to HDR needs Bayer or X-Trans RAWs", p.display()));
            }
            raw.apply_scaling().map_err(|e| e.to_string())?;
            Ok((raw, orientation))
        })
        .collect::<Result<_, String>>()?;
    let first = &decoded[0].0;
    let same = |r: &rawler::RawImage| {
        (r.width, r.height, &r.clean_make, &r.clean_model)
            == (first.width, first.height, &first.clean_make, &first.clean_model)
    };
    if !decoded.iter().all(|(r, _)| same(r)) {
        return Err("Merging to HDR needs photos from one camera, at one size".into());
    }
    let RawPhotometricInterpretation::Cfa(cfa) = &first.photometric else { unreachable!("checked above") };
    let period = cfa.cfa.width.max(cfa.cfa.height);
    let (w, h) = (first.width, first.height);
    // The mosaics move into the frames (no second copy); the reference's comes back merged.
    let frames: Vec<Frame> = decoded
        .iter_mut()
        .map(|(r, _)| match std::mem::replace(&mut r.data, RawImageData::Float(Vec::new())) {
            RawImageData::Float(pixels) => Frame { pixels },
            other => Frame { pixels: other.as_f32().into_owned() },
        })
        .collect();
    let merged = merge(&frames, w, h, period)?;
    drop(frames);

    let mut merged = merged;
    let (mut raw, orientation) = decoded.swap_remove(merged.reference);
    raw.data = RawImageData::Float(std::mem::take(&mut merged.pixels));
    let src = rawler::rawsource::RawSource::new(paths[merged.reference]).map_err(|e| e.to_string())?;
    let params = RawDecodeParams::default();
    let metadata = rawler::get_decoder(&src).and_then(|d| d.raw_metadata(&src, &params)).map_err(|e| e.to_string())?;
    // The source's own BaselineExposure (a DNG's) stays on top of the merge's.
    let source_ev = raw.dng_tags.get(&(DngTag::BaselineExposure as u16)).map_or(0.0, |v| v.force_f32(0));
    let write = || -> rawler::dng::writer::Result<()> {
        // Never over an existing file: two merges racing for one name, the second fails.
        let file = std::io::BufWriter::new(std::fs::File::create_new(out)?);
        let mut dng = DngWriter::new(file, DNG_VERSION_V1_4)?;
        let mut frame = dng.subframe_on_root(0);
        frame.raw_image(&raw, CropMode::Best, DngCompression::Uncompressed, DngPhotometricConversion::Original, 1)?;
        frame.finalize()?;
        dng.load_base_tags(&raw)?;
        dng.load_metadata(&metadata)?;
        dng.root_ifd_mut().add_tag(ExifTag::Orientation, orientation);
        // The merge is in the darkest frame's units: open it as the reference was exposed.
        let ev = merged.exposure[merged.reference].log2() + source_ev;
        dng.root_ifd_mut().add_tag(DngTag::BaselineExposure, SRational::new((ev * 10_000.0).round() as i32, 10_000));
        dng.close()?;
        Ok(())
    };
    if let Err(e) = write() {
        // A partial DNG is useless; an existing file (create_new failed) is not ours to remove.
        if !matches!(&e, rawler::dng::writer::DngError::Io(io) if io.kind() == std::io::ErrorKind::AlreadyExists) {
            let _ = std::fs::remove_file(out);
        }
        return Err(format!("Writing the HDR DNG failed: {e}"));
    }
    Ok(merged)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scene in the darkest frame's units: a ramp, a sky up to 4× past where the
    /// longest frame clips, fine texture to align on.
    fn scene(w: usize, h: usize) -> Vec<f32> {
        (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as f32, (i / w) as f32);
                // Flat 24-px cells of random level: aperiodic edges to align on, flat areas for the noise.
                let (cx, cy) = ((x / 24.0) as i64, (y / 24.0) as i64);
                let hash = (cx.wrapping_mul(73_856_093) ^ cy.wrapping_mul(19_349_663)) as u64;
                let texture = (hash.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 40) as f32 / (1u64 << 24) as f32;
                let sky = if y < h as f32 / 3.0 { 0.6 } else { 0.0 };
                (0.004 + 0.05 * x / w as f32) * (0.5 + texture) + sky
            })
            .collect()
    }

    /// `scene` exposed `e`× and shifted by `d` photosites, with Poisson–Gaussian noise, clipped at white.
    fn shoot(s: &[f32], w: usize, h: usize, e: f32, d: [i32; 2], seed: u64) -> Frame {
        let mut z = seed;
        let mut gauss = move || {
            // Two SplitMix64 uniforms, Box–Muller.
            let mut u = || {
                z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
                let mut v = z;
                v = (v ^ (v >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
                v = (v ^ (v >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
                ((v ^ (v >> 31)) >> 11) as f32 / (1u64 << 53) as f32
            };
            let (a, b) = (u().max(1e-9), u());
            (-2.0 * a.ln()).sqrt() * (std::f32::consts::TAU * b).cos()
        };
        let pixels = (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as i32 - d[0], (i / w) as i32 - d[1]);
                let v = s[y.clamp(0, h as i32 - 1) as usize * w + x.clamp(0, w as i32 - 1) as usize] * e;
                (v + (2e-4 * v + 1e-6).sqrt() * gauss()).clamp(0.0, 1.0)
            })
            .collect();
        Frame { pixels }
    }

    #[test]
    fn merge_recovers_the_scene_aligned_and_without_ghosts() {
        let (w, h) = (384, 256);
        let s = scene(w, h);
        // −2, 0, +2 EV around the reference; the bright ones shifted (handheld).
        let shots = [(1.0, [0, 0]), (4.0, [6, -4]), (16.0, [-10, 8])];
        let mut frames: Vec<Frame> =
            shots.iter().enumerate().map(|(k, &(e, d))| shoot(&s, w, h, e, d, k as u64 + 1)).collect();
        // A ghost: something bright crosses the longest frame only.
        for y in 150..170 {
            for x in 200..240 {
                frames[2].pixels[(y + 8) * w + x - 10] = 0.9;
            }
        }
        let m = merge(&frames, w, h, 2).unwrap();
        assert_eq!(m.reference, 1);
        // Shifts map the reference onto each frame.
        assert_eq!(m.shift, [[-6, 4], [0, 0], [-16, 12]]);
        for (e, want) in m.exposure.iter().zip([1.0, 4.0, 16.0]) {
            assert!((e / want - 1.0).abs() < 0.02, "{:?}", m.exposure);
        }
        // The merge is the scene in the darkest frame's units, laid on the reference (shifted 6, −4).
        // Shadows: RMS relative error of the merge, and of the reference frame alone.
        let (mut worst_sky, mut ghost, mut dark, mut alone, mut count) = (0f32, 0f32, 0f64, 0f64, 0f64);
        for y in 20..h - 20 {
            for x in 20..w - 20 {
                let (sx, sy) = (x as i32 - 6, y as i32 + 4);
                let want = s[sy as usize * w + sx as usize];
                let got = m.pixels[y * w + x];
                let err = (got - want).abs() / want;
                if (150..170).contains(&sy) && (200..240).contains(&sx) {
                    ghost = ghost.max(err);
                } else if want > 0.5 {
                    worst_sky = worst_sky.max(err);
                } else if want < 0.05 {
                    let solo = (frames[1].pixels[y * w + x] / 4.0 - want).abs() / want;
                    (dark, alone, count) = (dark + (err * err) as f64, alone + (solo * solo) as f64, count + 1.0);
                }
            }
        }
        // The sky clips in the two bright frames: it comes from the darkest alone.
        assert!(worst_sky < 0.1, "sky {worst_sky}");
        // Shadows mostly from the brightest frame: under half the reference's own noise.
        let (dark, alone) = ((dark / count).sqrt(), (alone / count).sqrt());
        assert!(dark < 0.5 * alone && dark < 0.05, "shadows {dark} vs the reference alone {alone}");
        assert!(ghost < 0.2, "ghost {ghost}");
    }

    /// A wide bracket (6 EV): ratios and shifts chain through neighbours, so the ends need
    /// not overlap the reference.
    #[test]
    fn wide_brackets_chain() {
        let (w, h) = (384, 256);
        let s = scene(w, h);
        let shots = [(1.0, [2, 0]), (4.0, [0, 0]), (16.0, [4, -2]), (64.0, [-6, 6])];
        let frames: Vec<Frame> =
            shots.iter().enumerate().map(|(k, &(e, d))| shoot(&s, w, h, e, d, k as u64 + 7)).collect();
        let m = merge(&frames, w, h, 2).unwrap();
        assert_eq!(m.reference, 2);
        for (e, want) in m.exposure.iter().zip([1.0, 4.0, 16.0, 64.0]) {
            assert!((e / want - 1.0).abs() < 0.03, "{:?}", m.exposure);
        }
        // Frame k at (x + d_k − d_ref) shows what the reference shows at x.
        assert_eq!(m.shift, [[-2, 2], [-4, 2], [0, 0], [-10, 8]]);
    }

    /// The same RAW three times is a no-op merge that reopens as a float DNG.
    /// `RAWVIBES_RAW=x.RW2 cargo test --release -p rawvibes-core merge_files_round_trip -- --ignored`.
    #[test]
    #[ignore]
    fn merge_files_round_trip() {
        let Ok(raw) = std::env::var("RAWVIBES_RAW") else { return };
        let p = Path::new(&raw);
        let out = std::env::temp_dir().join(format!("rawvibes-merge-{}.dng", std::process::id()));
        let m = merge_files(&[p, p, p], &out).unwrap();
        assert!(m.exposure.iter().all(|&e| (e - 1.0).abs() < 1e-3) && m.shift.iter().all(|&d| d == [0, 0]));
        let back = crate::raw::read_raw(&out).unwrap();
        let orig = crate::raw::read_raw(p).unwrap();
        assert_eq!(
            (back.width, back.height, back.crop, back.pattern),
            (orig.width, orig.height, orig.crop, orig.pattern)
        );
        let (
            crate::raw::Pixels::Bayer(a) | crate::raw::Pixels::XTrans(a),
            crate::raw::Pixels::Bayer(b) | crate::raw::Pixels::XTrans(b),
        ) = (&back.pixels, &orig.pixels)
        else {
            panic!("mosaics expected")
        };
        let worst = a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max);
        assert!(worst < 1e-4, "{worst}");
        assert_eq!(back.camera.baseline_ev, 0.0);
        std::fs::remove_file(out).unwrap();
    }
}
