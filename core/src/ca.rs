//! Automatic lateral chromatic-aberration correction on the Bayer CFA,
//! before demosaic: a port of RawTherapee's `CA_correct_RT` (Emil Martinec
//! 2008–2010; iterations and colour-shift avoidance by Ingo Weyrich 2018;
//! GPL-3.0-or-later). darktable's "raw chromatic aberrations" is the same
//! algorithm.
//!
//! Per 128 px tile, the R and B planes' sub-pixel shift against G is
//! measured where colour differences vary least; a 2-D polynomial is fitted
//! to the shifts over the image, and R/B are re-sampled at the fitted
//! optical positions through colour differences. RawTherapee's defaults:
//! automatic, two iterations, colour shift avoided.

// Index loops mirror RawTherapee's `[dir][colour]` arrays, for review against it.
#![allow(clippy::needless_range_loop)]

use rayon::prelude::*;

use crate::raw::{Pixels, RawData};

const TS: usize = 128;
const TSH: usize = TS / 2;
const BORDER: usize = 8;
const BORDER2: usize = 16;
/// Tile step: tiles overlap by `BORDER2`.
const STEP: usize = TS - BORDER2;
const V1: usize = TS;
const V2: usize = 2 * TS;
const V3: usize = 3 * TS;
const V4: usize = 4 * TS;
const EPS: f32 = 1e-5;
const EPS2: f32 = 1e-10;
/// RawTherapee's defaults: `caautoiterations`, Bayer `border`.
const ITERATIONS: usize = 2;
const CB: usize = 4;
const CA_AUTOSTRENGTH: f32 = 8.0;
/// Largest shift corrected, in pixels.
const BSLIM: f64 = 3.99f32 as f64;

/// `raw` with its lateral CA corrected; `None` when it does not apply: not a
/// 2×2 Bayer mosaic (X-Trans and linear RAWs arrive demosaiced, as RGB), or
/// too small for the tile statistics.
/// Whether `raw` can be corrected: a 2×2 Bayer mosaic at least two tiles across.
pub fn applies(raw: &RawData) -> bool {
    let [_, _, w, h] = raw.crop.map(|v| v as usize);
    matches!(raw.pixels, Pixels::Bayer(_)) && raw.pattern_size == [2, 2] && w >= 2 * TS && h >= 2 * TS
}

pub fn corrected(raw: &RawData) -> Option<RawData> {
    let Pixels::Bayer(mosaic) = &raw.pixels else { return None };
    let [cx, cy, w, h] = raw.crop.map(|v| v as usize);
    if !applies(raw) {
        return None;
    }
    let pat = |r: usize, c: usize| raw.pattern[((r + cy) % 2) * 2 + (c + cx) % 2] as usize;
    let cfa = [[pat(0, 0), pat(0, 1)], [pat(1, 0), pat(1, 1)]];
    // RawTherapee works on white-balanced data (its thresholds assume it).
    let wb = raw.camera.as_shot;
    let stride = raw.width as usize;
    let mut data = vec![0f32; w * h];
    data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let src = &mosaic[(y + cy) * stride + cx..];
        row.iter_mut().enumerate().for_each(|(x, v)| *v = src[x] * wb[cfa[y & 1][x & 1]]);
    });
    correct(&mut data, w, h, cfa);
    let mut out = vec![0f32; mosaic.len()];
    out.par_chunks_mut(stride).zip(mosaic.par_chunks(stride)).enumerate().for_each(|(y, (dst, src))| {
        dst.copy_from_slice(src);
        if let Some(y) = y.checked_sub(cy).filter(|&y| y < h) {
            for (x, v) in data[y * w..(y + 1) * w].iter().enumerate() {
                dst[x + cx] = v / wb[cfa[y & 1][x & 1]];
            }
        }
    });
    Some(RawData { pixels: Pixels::Bayer(out), camera: raw.camera.clone(), ..*raw })
}

/// A tile plane; fixed sizes let the compiler drop most bounds checks.
type Plane = [f32; TS * TS];
type Half = [f32; TS * TSH];

/// Per-thread tile buffers (RawTherapee's `bufferThr`).
struct Bufs {
    /// R and B at half index (in the first half), G at full index.
    rgb: [Box<Plane>; 3],
    /// High/low-pass filters of the diagnostic pass; the first two hold the
    /// colour differences and the shifted G of the correction pass.
    f: [Box<Half>; 6],
}

impl Bufs {
    fn new() -> Self {
        Self {
            rgb: std::array::from_fn(|_| Box::new([0.0; TS * TS])),
            f: std::array::from_fn(|_| Box::new([0.0; TS * TSH])),
        }
    }

    fn clear(&mut self) {
        self.rgb.iter_mut().for_each(|v| v.fill(0.0));
        self.f.iter_mut().for_each(|v| v.fill(0.0));
    }
}

/// Index of colour `c` at tile index `i`: R/B are stored at half index.
fn at(c: usize, i: usize) -> usize {
    if c == 1 { i } else { i >> 1 }
}

fn intp(a: f32, b: f32, c: f32) -> f32 {
    a * (b - c) + c
}

/// A tile's extent (RawTherapee's names): `rr1` × `cc1` tile pixels, the
/// rows/columns `rrmin..rrmax` × `ccmin..ccmax` inside the image.
struct Tile {
    top: i32,
    left: i32,
    vblock: usize,
    hblock: usize,
    rr1: usize,
    cc1: usize,
    rrmin: usize,
    rrmax: usize,
    ccmin: usize,
    ccmax: usize,
}

/// The image being corrected: `raw` (W × H, white-balanced, 1 = white).
struct Image<'a> {
    raw: &'a [f32],
    w: usize,
    h: usize,
    /// `w` rounded up to even: the stride of the half-size planes.
    width: usize,
    cfa: [[usize; 2]; 2],
}

impl Image<'_> {
    fn fc(&self, r: i32, c: i32) -> usize {
        self.cfa[(r & 1) as usize][(c & 1) as usize]
    }

    fn tile(&self, top: i32, left: i32) -> Tile {
        let (h, w) = (self.h as i32, self.w as i32);
        let bottom = (top + TS as i32).min(h + BORDER as i32);
        let right = (left + TS as i32).min(w + BORDER as i32);
        let (rr1, cc1) = (bottom - top, right - left);
        Tile {
            top,
            left,
            vblock: ((top + BORDER as i32) / STEP as i32 + 1) as usize,
            hblock: ((left + BORDER as i32) / STEP as i32 + 1) as usize,
            rr1: rr1 as usize,
            cc1: cc1 as usize,
            rrmin: if top < 0 { BORDER } else { 0 },
            rrmax: (if bottom > h { h - top } else { rr1 }) as usize,
            ccmin: if left < 0 { BORDER } else { 0 },
            ccmax: (if right > w { w - left } else { cc1 }) as usize,
        }
    }

    fn lefts(&self) -> impl Iterator<Item = i32> {
        (-(BORDER as i32)..self.w as i32).step_by(STEP)
    }

    /// Loads tile `t` from the CFA, borders mirrored as RawTherapee does;
    /// with `gtmp`, G at R/B sites from the diagnostic pass too. The fills
    /// write disjoint areas, so their order differs from RawTherapee's.
    // RawTherapee's first pass fills `border` rows/columns at the bottom/right
    // even past the tile (writing into the next buffer); clamped as its second pass does.
    fn load(&self, b: &mut Bufs, t: &Tile, gtmp: Option<&[f32]>) {
        // RawTherapee zeroes every tile; a full one is overwritten wherever
        // it is read (a few unused filter rows aside), so only partial ones are.
        if t.rr1 < TS || t.cc1 < TS {
            b.clear();
        }
        let [r0, g, r2] = &mut b.rgb;
        let (raw, w, width) = (self.raw, self.w, self.width);
        let mut put = |c: usize, ti: usize, row: usize, col: usize| {
            let v = raw[row * w + col];
            match c {
                1 => g[ti] = v,
                _ => {
                    (if c == 0 { &mut *r0 } else { &mut *r2 })[ti >> 1] = v;
                    if let Some(gt) = gtmp {
                        g[ti] = gt[(row * width + col) >> 1];
                    }
                }
            }
        };
        let fc = |r: usize, c: usize| self.fc(r as i32, c as i32);
        let (top, left) = (t.top as isize, t.left as isize);
        let abs = |v: isize, o: usize| (v + o as isize) as usize;
        for rr in t.rrmin..t.rrmax {
            for cc in t.ccmin..t.ccmax {
                put(fc(rr, cc), rr * TS + cc, abs(top, rr), abs(left, cc));
            }
        }
        let (h, bot, rgt) = (self.h, (t.rr1 - t.rrmax).min(BORDER), (t.cc1 - t.ccmax).min(BORDER));
        if t.rrmax < t.rr1 {
            for rr in 0..bot {
                for cc in t.ccmin..t.ccmax {
                    put(fc(rr, cc), (t.rrmax + rr) * TS + cc, h - rr - 2, abs(left, cc));
                }
            }
        }
        if t.ccmax < t.cc1 {
            for rr in t.rrmin..t.rrmax {
                for cc in 0..rgt {
                    put(fc(rr, cc), rr * TS + t.ccmax + cc, abs(top, rr), width - cc - 2);
                }
            }
        }
        let corners = [
            (t.rrmin > 0 && t.ccmin > 0, BORDER, BORDER, None, None),
            (t.rrmax < t.rr1 && t.ccmax < t.cc1, bot, rgt, Some(t.rrmax), Some(t.ccmax)),
            (t.rrmin > 0 && t.ccmax < t.cc1, BORDER, rgt, None, Some(t.ccmax)),
            (t.rrmax < t.rr1 && t.ccmin > 0, bot, BORDER, Some(t.rrmax), None),
        ];
        for (on, nr, nc, down, right) in corners {
            if !on {
                continue;
            }
            for rr in 0..nr {
                for cc in 0..nc {
                    let (trr, row) = down.map_or((rr, BORDER2 - rr), |m| (m + rr, h - rr - 2));
                    let (tcc, col) = right.map_or((cc, BORDER2 - cc), |m| (m + cc, width - cc - 2));
                    put(fc(rr, cc), trr * TS + tcc, row, col);
                }
            }
        }
        // Mirrored top and left borders.
        let mut mirror = |c: usize, dst: usize, src: usize| {
            let p = if c == 1 {
                &mut *g
            } else if c == 0 {
                &mut *r0
            } else {
                &mut *r2
            };
            p[at(c, dst)] = p[at(c, src)];
            if gtmp.is_some() {
                g[dst] = g[src];
            }
        };
        if t.rrmin > 0 {
            for rr in 0..BORDER {
                for cc in t.ccmin..t.ccmax {
                    mirror(fc(rr, cc), rr * TS + cc, (BORDER2 - rr) * TS + cc);
                }
            }
        }
        if t.ccmin > 0 {
            for rr in t.rrmin..t.rrmax {
                for cc in 0..BORDER {
                    mirror(fc(rr, cc), rr * TS + cc, rr * TS + BORDER2 - cc);
                }
            }
        }
    }
}

/// What the diagnostic pass measured on one tile.
struct Diag {
    vblock: usize,
    hblock: usize,
    weight: f32,
    /// `[colour][dir]` (0 R, 1 B; 0 vertical, 1 horizontal).
    shift: [[f32; 2]; 2],
}

/// Shift statistics of the tiles whose shift is under 2 px: `[dir][colour]`.
#[derive(Default, Clone, Copy)]
struct Stats {
    ave: [[f32; 2]; 2],
    sqave: [[f32; 2]; 2],
    denom: [[f32; 2]; 2],
}

/// G interpolated at the R/B sites of a loaded tile (gradient-weighted).
fn interpolate_g(im: &Image, b: &mut Bufs, t: &Tile) {
    assert!(t.rr1 <= TS && t.cc1 <= TS);
    let [r0, g, r2] = &mut b.rgb;
    for rr in 3..t.rr1 - 3 {
        let mut cc = 3 + (im.fc(rr as i32, 3) & 1);
        let c = im.fc(rr as i32, cc as i32);
        let cv: &Plane = if c == 0 { r0 } else { r2 };
        while cc < t.cc1 - 3 {
            let i = rr * TS + cc;
            let h = i >> 1;
            let sq = |v: f32| 1.0 / (v * v);
            let wtu = sq(EPS
                + (g[i + V1] - g[i - V1]).abs()
                + (cv[h] - cv[(i - V2) >> 1]).abs()
                + (g[i - V1] - g[i - V3]).abs());
            let wtd = sq(EPS
                + (g[i - V1] - g[i + V1]).abs()
                + (cv[h] - cv[(i + V2) >> 1]).abs()
                + (g[i + V1] - g[i + V3]).abs());
            let wtl =
                sq(EPS + (g[i + 1] - g[i - 1]).abs() + (cv[h] - cv[(i - 2) >> 1]).abs() + (g[i - 1] - g[i - 3]).abs());
            let wtr =
                sq(EPS + (g[i - 1] - g[i + 1]).abs() + (cv[h] - cv[(i + 2) >> 1]).abs() + (g[i + 1] - g[i + 3]).abs());
            g[i] = (wtu * g[i - V1] + wtd * g[i + V1] + wtl * g[i - 1] + wtr * g[i + 1]) / (wtu + wtd + wtl + wtr);
            cc += 2;
        }
    }
}

/// Diagnostic pass on one loaded tile: its R/B shift against G.
fn diagnose(im: &Image, b: &mut Bufs, t: &Tile, stats: &mut Stats) -> Diag {
    interpolate_g(im, b, t);
    let [r0, g, r2] = &b.rgb;
    let [rbhpfh, rbhpfv, rblpfh, rblpfv, grblpfh, grblpfv] = &mut b.f;
    assert!(t.rr1 <= TS && t.cc1 <= TS);
    for rr in 4..t.rr1 - 4 {
        let mut cc = 4 + (im.fc(rr as i32, 2) & 1);
        let c = im.fc(rr as i32, cc as i32);
        let cv: &Plane = if c == 0 { r0 } else { r2 };
        while cc < t.cc1 - 4 {
            let i = rr * TS + cc;
            let h = i >> 1;
            let d = |j: usize| g[j] - cv[j >> 1];
            let (d0, dvp, dvm, dhp, dhm) = (d(i), d(i + V4), d(i - V4), d(i + 4), d(i - 4));
            rbhpfv[h] = ((d0 - dvp).abs() + (dvm - d0).abs() - (dvm - dvp).abs()).abs();
            rbhpfh[h] = ((d0 - dhp).abs() + (dhm - d0).abs() - (dhm - dhp).abs()).abs();
            let glpfv = 2.0 * g[i] + g[i + V2] + g[i - V2];
            let glpfh = 2.0 * g[i] + g[i + 2] + g[i - 2];
            let clpfv = 2.0 * cv[h] + cv[(i + V2) >> 1] + cv[(i - V2) >> 1];
            let clpfh = 2.0 * cv[h] + cv[(i + 2) >> 1] + cv[(i - 2) >> 1];
            rblpfv[h] = 0.25 * (glpfv - clpfv).abs();
            rblpfh[h] = 0.25 * (glpfh - clpfh).abs();
            grblpfv[h] = 0.25 * (glpfv + clpfv);
            grblpfh[h] = 0.25 * (glpfh + clpfh);
            cc += 2;
        }
    }
    // Along line segments, the point minimising the colour variance over the tile;
    // colour differences are quadratic in the interpolation position. coeff[dir][k][colour].
    let mut coeff = [[[0f32; 2]; 3]; 2];
    for rr in 8..t.rr1 - 8 {
        let mut cc = 8 + (im.fc(rr as i32, 2) & 1);
        let c = im.fc(rr as i32, cc as i32);
        let cv: &Plane = if c == 0 { r0 } else { r2 };
        let mut row = [[0f32; 3]; 2];
        while cc < t.cc1 - 8 {
            let i = rr * TS + cc;
            let h = i >> 1;
            let deltgrb = cv[h] - g[i];
            let gdiffv =
                (g[i + TS] - g[i - TS]) + 0.3 * (g[i + TS + 1] - g[i - TS + 1] + g[i + TS - 1] - g[i - TS - 1]);
            let gradwtv = (rbhpfv[h] + 0.5 * (rbhpfv[h + 1] + rbhpfv[h - 1])) * (grblpfv[h - V1] + grblpfv[h + V1])
                / (EPS + 0.1 * (grblpfv[h - V1] + grblpfv[h + V1]) + rblpfv[h - V1] + rblpfv[h + V1]);
            let gdiffh = (g[i + 1] - g[i - 1]) + 0.3 * (g[i + 1 + TS] - g[i - 1 + TS] + g[i + 1 - TS] - g[i - 1 - TS]);
            let gradwth = (rbhpfh[h] + 0.5 * (rbhpfh[h + V1] + rbhpfh[h - V1])) * (grblpfh[h - 1] + grblpfh[h + 1])
                / (EPS + 0.1 * (grblpfh[h - 1] + grblpfh[h + 1]) + rblpfh[h - 1] + rblpfh[h + 1]);
            for (acc, (wt, gd)) in row.iter_mut().zip([(gradwtv, gdiffv), (gradwth, gdiffh)]) {
                acc[0] += wt * deltgrb * deltgrb;
                acc[1] += wt * gd * deltgrb;
                acc[2] += wt * gd * gd;
            }
            cc += 2;
        }
        for dir in 0..2 {
            for k in 0..3 {
                coeff[dir][k][c >> 1] += row[dir][k];
            }
        }
    }
    for k in coeff.iter_mut().flatten() {
        k.iter_mut().for_each(|v| *v *= 0.25);
    }
    for dir in &mut coeff {
        dir[1].iter_mut().for_each(|v| *v *= 0.3125);
        dir[2].iter_mut().for_each(|v| *v *= 0.3125 * 0.3125);
    }
    let mut d = Diag { vblock: t.vblock, hblock: t.hblock, weight: 0.0, shift: [[0.0; 2]; 2] };
    for c in 0..2 {
        for dir in 0..2 {
            // The approximate optical location of the R/B pixels. As in
            // RawTherapee, the last colour/direction sets the block weight.
            let s = if coeff[dir][2][c] > EPS2 {
                d.weight = coeff[dir][2][c] / (EPS + coeff[dir][0][c]);
                coeff[dir][1][c] / coeff[dir][2][c]
            } else {
                d.weight = 0.0;
                17.0
            };
            if s.abs() < 2.0 {
                stats.ave[dir][c] += s;
                stats.sqave[dir][c] += s * s;
                stats.denom[dir][c] += 1.0;
            }
            d.shift[c][dir] = s;
        }
    }
    d
}

/// Correction pass on one loaded tile (G at R/B sites from the diagnostic
/// pass): R/B re-sampled at `shifts` (`[colour][dir]`).
fn correct_tile(im: &Image, b: &mut Bufs, t: &Tile, shifts: [[f64; 2]; 2]) {
    assert!(t.rr1 <= TS && t.cc1 <= TS);
    let [r0, g, r2] = &mut b.rgb;
    let [grbdiff, gshift, ..] = &mut b.f;
    let (mut svf, mut svc, mut shf, mut shc) = ([0isize; 3], [0isize; 3], [0isize; 3], [0isize; 3]);
    let (mut svfrac, mut shfrac, mut dir) = ([0f32; 3], [0f32; 3], [[0isize; 3]; 2]);
    for c in [0, 2] {
        let s = shifts[c >> 1];
        for (k, (fl, ce, frac)) in
            [(&mut svf, &mut svc, &mut svfrac), (&mut shf, &mut shc, &mut shfrac)].into_iter().enumerate()
        {
            fl[c] = (s[k] as f32).floor() as isize;
            ce[c] = (s[k] as f32).ceil() as isize;
            if s[k] < 0.0 {
                std::mem::swap(&mut fl[c], &mut ce[c]);
            }
            frac[c] = (s[k] - fl[c] as f64).abs() as f32;
            dir[k][c] = if s[k] > 0.0 { 2 } else { -2 };
        }
    }
    let ix = |v: isize| v as usize;
    for rr in 4..t.rr1 - 4 {
        let mut cc = 4 + (im.fc(rr as i32, 2) & 1);
        let c = im.fc(rr as i32, cc as i32);
        let cv: &Plane = if c == 0 { r0 } else { r2 };
        let (r, sv, sc) = (rr as isize, svf[c], svc[c]);
        while cc < t.cc1 - 4 {
            let x = cc as isize;
            let h = (rr * TS + cc) >> 1;
            let ghf =
                intp(shfrac[c], g[ix((r + sv) * TS as isize + x + shc[c])], g[ix((r + sv) * TS as isize + x + shf[c])]);
            let ghc =
                intp(shfrac[c], g[ix((r + sc) * TS as isize + x + shc[c])], g[ix((r + sc) * TS as isize + x + shf[c])]);
            let gint = intp(svfrac[c], ghc, ghf);
            grbdiff[h] = gint - cv[h];
            gshift[h] = gint;
            cc += 2;
        }
    }
    for c in [0, 2] {
        shfrac[c] /= 2.0;
        svfrac[c] /= 2.0;
    }
    for rr in 8..t.rr1 - 8 {
        let mut cc = 8 + (im.fc(rr as i32, 2) & 1);
        let c = im.fc(rr as i32, cc as i32);
        let cv: &mut Plane = if c == 0 { r0 } else { r2 };
        let (d0, d1) = (dir[0][c], dir[1][c]);
        let up = ((rr as isize - d0) * TS as isize) as usize;
        while cc < t.cc1 - 8 {
            let i = rr * TS + cc;
            let h = i >> 1;
            let left = ix(i as isize - d1) >> 1;
            let (uh, ul) = ((up + cc) >> 1, ix((up + cc) as isize - d1) >> 1);
            let old = g[i] - cv[h];
            let ghf = intp(shfrac[c], grbdiff[left], grbdiff[h]);
            let ghc = intp(shfrac[c], grbdiff[ul], grbdiff[uh]);
            let mut int = intp(svfrac[c], ghc, ghf);
            let rb = g[i] - int;
            if (rb - cv[h]).abs() < 0.25 * (rb + cv[h]) {
                if old.abs() > int.abs() {
                    cv[h] = rb;
                }
            } else {
                // Gradient weights from the difference between G at the shift points and at the grid point.
                let p = |j: usize| 1.0 / (EPS + (g[i] - gshift[j]).abs());
                let (p0, p1, p2, p3) = (p(h), p(left), p(uh), p(ul));
                int =
                    (p0 * grbdiff[h] + p1 * grbdiff[left] + p2 * grbdiff[uh] + p3 * grbdiff[ul]) / (p0 + p1 + p2 + p3);
                if old.abs() > int.abs() {
                    cv[h] = g[i] - int;
                }
            }
            // The interpolation overshot the correction: just desaturate.
            if old * int < 0.0 {
                cv[h] = g[i] - 0.5 * (old + int);
            }
            cc += 2;
        }
    }
}

/// Corrects `raw` (W × H, white-balanced, 1 = white) in place.
fn correct(raw: &mut [f32], w: usize, h: usize, cfa: [[usize; 2]; 2]) {
    let width = w + (w & 1);
    let fc = |r: usize, c: usize| cfa[r & 1][c & 1];
    // Colour-shift avoidance compares against the uncorrected R/B.
    let hw = (w + 1 - 2 * CB) / 2;
    let hh = (h + 1 - 2 * CB) / 2;
    let mut oldraw = vec![0f32; hw * (h - 2 * CB)];
    oldraw.par_chunks_mut(hw).enumerate().for_each(|(r, o)| {
        let i = r + CB;
        for j in (CB + (fc(i, 0) & 1)..w - CB).step_by(2) {
            o[(j - CB) / 2] = raw[i * w + j];
        }
    });
    let vblsz = blocks(h);
    let hblsz = blocks(width);
    let rows = (h + BORDER).div_ceil(STEP);
    // Tile rows own disjoint `STEP`-row bands of these half-size planes.
    let band = STEP * width / 2;
    let mut gtmp = vec![0f32; rows * band];
    let mut rawtmp = vec![0f32; rows * band];
    let mut blockwt = vec![0f32; vblsz * hblsz];
    let mut blockshifts = vec![[[0f32; 2]; 2]; vblsz * hblsz];
    let mut fitparams = [[[0f64; 16]; 2]; 2];
    let mut processpasstwo = true;
    for _ in 0..ITERATIONS {
        if !processpasstwo {
            break;
        }
        let mut polyord = 4;
        let im = Image { raw, w, h, width, cfa };
        // Diagnostic pass, also keeping G at the R/B sites.
        let diags: Vec<(Vec<Diag>, Stats)> = gtmp
            .par_chunks_mut(band)
            .enumerate()
            .map(|(k, out)| {
                let (mut b, mut stats, mut diags) = (Bufs::new(), Stats::default(), Vec::new());
                let top = (k * STEP) as i32 - BORDER as i32;
                for left in im.lefts() {
                    let t = im.tile(top, left);
                    im.load(&mut b, &t, None);
                    diags.push(diagnose(&im, &mut b, &t, &mut stats));
                    store(&im, &t, out, k * band, |_, i| b.rgb[1][i]);
                }
                (diags, stats)
            })
            .collect();
        let mut st = Stats::default();
        for (ds, s) in &diags {
            for d in ds {
                let i = d.vblock * hblsz + d.hblock;
                blockwt[i] = d.weight;
                blockshifts[i] = d.shift;
            }
            for dir in 0..2 {
                for c in 0..2 {
                    st.ave[dir][c] += s.ave[dir][c];
                    st.sqave[dir][c] += s.sqave[dir][c];
                    st.denom[dir][c] += s.denom[dir][c];
                }
            }
        }
        let mut blockvar = [[0f32; 2]; 2];
        for dir in 0..2 {
            for c in 0..2 {
                if st.denom[dir][c] != 0.0 {
                    let mean = st.ave[dir][c] / st.denom[dir][c];
                    blockvar[dir][c] = st.sqave[dir][c] / st.denom[dir][c] - mean * mean;
                } else {
                    processpasstwo = false;
                }
            }
        }
        if processpasstwo {
            match fit(&mut blockshifts, &blockwt, vblsz, hblsz, blockvar) {
                Some((p, ord)) => (fitparams, polyord) = (p, ord),
                None => processpasstwo = false,
            }
        }
        if processpasstwo {
            let (gtmp, im) = (&gtmp, &im);
            rawtmp.par_chunks_mut(band).enumerate().for_each(|(k, out)| {
                let mut b = Bufs::new();
                let top = (k * STEP) as i32 - BORDER as i32;
                for left in im.lefts() {
                    let t = im.tile(top, left);
                    im.load(&mut b, &t, Some(gtmp));
                    let mut shifts = [[0f64; 2]; 2];
                    let mut pv = 1.0;
                    for i in 0..polyord {
                        let mut ph = pv;
                        for j in 0..polyord {
                            for (c, s) in shifts.iter_mut().enumerate() {
                                for (dir, v) in s.iter_mut().enumerate() {
                                    *v += ph * fitparams[c][dir][polyord * i + j];
                                }
                            }
                            ph *= t.hblock as f64;
                        }
                        pv *= t.vblock as f64;
                    }
                    let shifts = shifts.map(|s| s.map(|v| v.clamp(-BSLIM, BSLIM)));
                    correct_tile(im, &mut b, &t, shifts);
                    store(im, &t, out, k * band, |c, i| b.rgb[c][i >> 1]);
                }
            });
            raw.par_chunks_mut(w).enumerate().skip(CB).take(h - 2 * CB).for_each(|(row, line)| {
                for col in (CB + (fc(row, 0) & 1)..width - CB).step_by(2) {
                    line[col] = rawtmp[(row * width + col) >> 1].max(0.0);
                }
            });
        }
        avoid_colour_shift(raw, w, h, cfa, &oldraw, hw, hh);
    }
}

/// Copies the tile core (rows/columns `BORDER..` from each edge) at the
/// R/B sites, `value(colour, tile index)`, into `out`: the half-size band
/// starting at half index `base`.
fn store(im: &Image, t: &Tile, out: &mut [f32], base: usize, value: impl Fn(usize, usize) -> f32) {
    for rr in BORDER..t.rr1 - BORDER {
        let row = (t.top + rr as i32) as usize;
        let first = (t.left + BORDER as i32).max(0) as usize;
        let end = ((t.left + (t.cc1 - BORDER) as i32) as usize).min(im.width);
        let mut col = first + (im.fc(row as i32, first as i32) & 1);
        let c = im.fc(row as i32, col as i32);
        while col < end {
            out[((row * im.width + col) >> 1) - base] = value(c, rr * TS + (col as i32 - t.left) as usize);
            col += 2;
        }
    }
}

/// Blocks along a side of `n` pixels, with a border block each side.
fn blocks(n: usize) -> usize {
    let z = usize::from((n + BORDER2).is_multiple_of(STEP));
    ((n + BORDER2) as f32 / STEP as f32 + 2.0 + z as f32).ceil() as usize
}

/// Fits the polynomial to the 3×3-median block shifts, within
/// `CA_AUTOSTRENGTH` variances of zero: `fitparams[colour][dir]` and the
/// polynomial order, or `None` when too few blocks or no solution.
fn fit(
    shifts: &mut [[[f32; 2]; 2]],
    wt: &[f32],
    vblsz: usize,
    hblsz: usize,
    var: [[f32; 2]; 2],
) -> Option<([[[f64; 16]; 2]; 2], usize)> {
    // Border blocks copy their inner neighbours.
    for v in 1..vblsz - 1 {
        shifts[v * hblsz] = shifts[v * hblsz + 2];
        shifts[v * hblsz + hblsz - 1] = shifts[v * hblsz + hblsz - 3];
    }
    for hb in 0..hblsz {
        shifts[hb] = shifts[2 * hblsz + hb];
        shifts[(vblsz - 1) * hblsz + hb] = shifts[(vblsz - 3) * hblsz + hb];
    }
    let (polyord, numpar) = (4, 16);
    let mut polymat = [[[0f64; 256]; 2]; 2];
    let mut shiftmat = [[[0f64; 16]; 2]; 2];
    let mut numblox = [0usize; 2];
    for v in 1..vblsz - 1 {
        for hb in 1..hblsz - 1 {
            for c in 0..2 {
                let mut bs = [0f32; 2];
                for (dir, b) in bs.iter_mut().enumerate() {
                    let mut p: [f32; 9] =
                        std::array::from_fn(|k| shifts[(v + k / 3 - 1) * hblsz + hb + k % 3 - 1][c][dir]);
                    *b = crate::noise::median(&mut p);
                }
                if bs[0] * bs[0] > CA_AUTOSTRENGTH * var[0][c] || bs[1] * bs[1] > CA_AUTOSTRENGTH * var[1][c] {
                    continue;
                }
                numblox[c] += 1;
                let w = wt[v * hblsz + hb] as f64;
                for dir in 0..2 {
                    let mut pvi = 1.0;
                    for i in 0..polyord {
                        let mut phi = 1.0;
                        for j in 0..polyord {
                            let mut pv = pvi;
                            for m in 0..polyord {
                                let mut ph = phi;
                                for n in 0..polyord {
                                    polymat[c][dir][numpar * (polyord * i + j) + (polyord * m + n)] += pv * ph * w;
                                    ph *= hb as f64;
                                }
                                pv *= v as f64;
                            }
                            shiftmat[c][dir][polyord * i + j] += pvi * phi * bs[dir] as f64 * w;
                            phi *= hb as f64;
                        }
                        pvi *= v as f64;
                    }
                }
            }
        }
    }
    let blox = numblox[0].min(numblox[1]);
    if blox < 10 {
        return None;
    }
    // Too few blocks: linear fit. As in RawTherapee, its 4×4 system is the
    // first 16 entries of the quartic one's.
    let (polyord, numpar) = if blox < 32 { (2, 4) } else { (polyord, numpar) };
    let mut fp = [[[0f64; 16]; 2]; 2];
    for c in 0..2 {
        for dir in 0..2 {
            if !lin_eq_solve(numpar, &mut polymat[c][dir], &mut shiftmat[c][dir], &mut fp[c][dir]) {
                return None;
            }
        }
    }
    Some((fp, polyord))
}

/// Gaussian elimination with partial pivoting (Henry Guennadi Levkin's
/// solver in RawTherapee, pivot search kept as is); false when singular.
fn lin_eq_solve(n: usize, a: &mut [f64], b: &mut [f64], x: &mut [f64]) -> bool {
    for k in 0..n - 1 {
        let mut max = a[k * n + k].abs();
        let mut m = k;
        for i in k + 1..n {
            if max < a[i * n + k].abs() {
                max = a[i * n + k];
                m = i;
            }
        }
        if m != k {
            for i in k..n {
                a.swap(k * n + i, m * n + i);
            }
            b.swap(k, m);
        }
        if a[k * n + k] == 0.0 {
            return false;
        }
        for j in k + 1..n {
            let acc = -a[j * n + k] / a[k * n + k];
            for i in k..n {
                a[j * n + i] += acc * a[k * n + i];
            }
            b[j] += acc * b[k];
        }
    }
    for k in (0..n).rev() {
        x[k] = b[k];
        for i in k + 1..n {
            x[k] -= a[k * n + i] * x[i];
        }
        x[k] /= a[k * n + k];
    }
    true
}

/// Scales the corrected R/B by the Gaussian-blurred (σ 30) ratio of the
/// uncorrected to the corrected values: what the correction moved locally,
/// not the colour of whole areas.
fn avoid_colour_shift(raw: &mut [f32], w: usize, h: usize, cfa: [[usize; 2]; 2], old: &[f32], hw: usize, hh: usize) {
    let fc = |r: usize, c: usize| cfa[r & 1][c & 1];
    // RawTherapee's "1" on a 0–65535 scale.
    let floor = 1.0 / 65535.0;
    let mut factor = [vec![0f32; hw * hh], vec![0f32; hw * hh]];
    let raw_ref = &*raw;
    for (p, plane) in factor.iter_mut().enumerate() {
        // Factor row k: the row of image rows 2k, 2k + 1 with this colour.
        plane.par_chunks_mut(hw).enumerate().for_each(|(k, f)| {
            for i in (2 * k..2 * k + 2).filter(|&i| i < h - 2 * CB) {
                let first = fc(i, 0) & 1;
                if fc(i, first) >> 1 != p {
                    continue;
                }
                for j in (first..w - 2 * CB).step_by(2) {
                    let (new, o) = (raw_ref[(i + CB) * w + j + CB], old[i * hw + j / 2]);
                    f[j / 2] = if new <= floor || o <= floor { 1.0 } else { (o / new).clamp(0.5, 2.0) };
                }
            }
        });
    }
    if h % 2 == 1 {
        // Odd height: the last row of factors is unset; use the one before.
        for f in &mut factor {
            let (a, b) = f.split_at_mut((hh - 1) * hw);
            b.copy_from_slice(&a[(hh - 2) * hw..]);
        }
    }
    if w % 2 == 1 {
        let ng_row = 1 - (fc(0, 0) & 1);
        let f = &mut factor[fc(ng_row, fc(ng_row, 0) & 1) >> 1];
        for i in 0..hh {
            f[i * hw + hw - 1] = f[i * hw + hw - 2];
        }
    }
    for f in &mut factor {
        gaussian_blur(f, hw, hh, 30.0);
    }
    raw.par_chunks_mut(w).skip(CB).take(h - 2 * CB).enumerate().for_each(|(i, row)| {
        let first = fc(i, 0) & 1;
        let f = &factor[fc(i, first) >> 1][(i / 2) * hw..];
        for j in (first..w - 2 * CB).step_by(2) {
            row[j + CB] *= f[j / 2];
        }
    });
}

/// Young–van Vliet recursive Gaussian with Triggs–Sdika boundaries, in
/// double precision (RawTherapee's `gaussianBlur` for large σ), rows then
/// columns, `STRIP` lines at a time as interleaved lanes.
fn gaussian_blur(p: &mut [f32], w: usize, h: usize, sigma: f64) {
    const STRIP: usize = 8;
    p.par_chunks_mut(STRIP * w).for_each_init(Vec::new, |t, rows| {
        let n = rows.len() / w;
        t.resize(n * w, 0.0);
        for (k, row) in rows.chunks(w).enumerate() {
            row.iter().enumerate().for_each(|(x, &a)| t[x * n + k] = a as f64);
        }
        yvv(t, n, sigma);
        for (k, row) in rows.chunks_mut(w).enumerate() {
            row.iter_mut().enumerate().for_each(|(x, a)| *a = t[x * n + k] as f32);
        }
    });
    let strips: Vec<Vec<f32>> = (0..w.div_ceil(STRIP))
        .into_par_iter()
        .map_init(Vec::new, |t, s| {
            let (x0, n) = (s * STRIP, STRIP.min(w - s * STRIP));
            t.clear();
            p.chunks(w).for_each(|row| t.extend(row[x0..x0 + n].iter().map(|&a| a as f64)));
            yvv(t, n, sigma);
            t.iter().map(|&a| a as f32).collect()
        })
        .collect();
    p.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (s, v) in strips.iter().enumerate() {
            let n = v.len() / h;
            row[s * STRIP..s * STRIP + n].copy_from_slice(&v[y * n..(y + 1) * n]);
        }
    });
}

/// [`gaussian_blur`] in place along `lanes` interleaved lines (element `j`
/// of lane `k` at `j * lanes + k`): RawTherapee's `gaussHorizontal`/`gaussVertical`.
fn yvv(t: &mut [f64], lanes: usize, sigma: f64) {
    let q = if sigma < 2.5 { 3.97156 - 4.14554 * (1.0 - 0.26891 * sigma).sqrt() } else { 0.98711 * sigma - 0.96330 };
    let b0 = 1.57825 + 2.44413 * q + 1.4281 * q * q + 0.422205 * q * q * q;
    let (b1, b2, b3) = (
        (2.44413 * q + 2.85619 * q * q + 1.26661 * q * q * q) / b0,
        (-1.4281 * q * q - 1.26661 * q * q * q) / b0,
        (0.422205 * q * q * q) / b0,
    );
    let bb = 1.0 - (b1 + b2 + b3);
    let norm = (1.0 + b1 - b2 + b3) * (1.0 + b2 + (b1 - b3) * b3);
    let m = [
        [-b3 * b1 + 1.0 - b3 * b3 - b2, (b3 + b1) * (b2 + b3 * b1), b3 * (b1 + b3 * b2)],
        [b1 + b3 * b2, -(b2 - 1.0) * (b2 + b3 * b1), -(b3 * b1 + b3 * b3 + b2 - 1.0) * b3],
        [
            b3 * b1 + b2 + b1 * b1 - b2 * b2,
            b1 * b2 + b3 * b2 * b2 - b1 * b3 * b3 - b3 * b3 * b3 - b3 * b2 + b3,
            b3 * (b1 + b3 * b2),
        ],
    ]
    .map(|r| r.map(|v| v / norm));
    let (l, n) = (lanes, t.len() / lanes);
    let last = t[(n - 1) * l..].to_vec();
    for k in 0..l {
        let s0 = t[k];
        t[k] = (bb + b1 + b2 + b3) * s0;
        t[l + k] = bb * t[l + k] + b1 * t[k] + (b2 + b3) * s0;
        t[2 * l + k] = bb * t[2 * l + k] + b1 * t[l + k] + b2 * t[k] + b3 * s0;
    }
    for j in 3..n {
        let (done, cur) = t.split_at_mut(j * l);
        let (p1, p2, p3) = (&done[(j - 1) * l..], &done[(j - 2) * l..], &done[(j - 3) * l..]);
        for k in 0..l {
            cur[k] = bb * cur[k] + b1 * p1[k] + b2 * p2[k] + b3 * p3[k];
        }
    }
    for (k, &u) in last.iter().enumerate() {
        let at = |j: usize| t[(n - j) * l + k] - u;
        let e = |r: [f64; 3]| u + r[0] * at(1) + r[1] * at(2) + r[2] * at(3);
        let (wm1, w0, wp1) = (e(m[0]), e(m[1]), e(m[2]));
        t[(n - 1) * l + k] = wm1;
        t[(n - 2) * l + k] = bb * t[(n - 2) * l + k] + b1 * wm1 + b2 * w0 + b3 * wp1;
        t[(n - 3) * l + k] = bb * t[(n - 3) * l + k] + b1 * t[(n - 2) * l + k] + b2 * wm1 + b3 * w0;
    }
    for j in (0..n - 3).rev() {
        let (cur, next) = t.split_at_mut((j + 1) * l);
        let cur = &mut cur[j * l..];
        for k in 0..l {
            cur[k] = bb * cur[k] + b1 * next[k] + b2 * next[l + k] + b3 * next[2 * l + k];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::develop::Camera;

    const W: usize = 1024;
    const H: usize = 768;

    /// A grey scene of soft-edged squares (about 1.7 px edges).
    fn scene(x: f32, y: f32) -> f32 {
        let s = |t: f32, p: f32| (3.0 * (std::f32::consts::TAU * t / p).sin()).tanh();
        0.45 + 0.3 * s(x, 37.0) * s(y, 29.0)
    }

    /// RGGB mosaic of `scene` with R and B magnified about the centre by `1 + k`.
    fn mosaic(kr: f32, kb: f32) -> RawData {
        let (cx, cy) = (W as f32 / 2.0, H as f32 / 2.0);
        let px = (0..H * W)
            .map(|i| {
                let (x, y) = ((i % W) as f32, (i / W) as f32);
                let k = match (i / W % 2, i % W % 2) {
                    (0, 0) => kr,
                    (1, 1) => kb,
                    _ => 0.0,
                };
                scene(cx + (x - cx) * (1.0 + k), cy + (y - cy) * (1.0 + k))
            })
            .collect();
        let mut pattern = [0; 36];
        pattern[..4].copy_from_slice(&[0, 1, 1, 2]);
        RawData {
            pixels: Pixels::Bayer(px),
            width: W as u32,
            height: H as u32,
            pattern,
            pattern_size: [2, 2],
            crop: [0, 0, W as u32, H as u32],
            orientation: 1,
            camera: Camera::neutral(),
        }
    }

    /// Misregistration of R and B against the scene in the corners (beyond
    /// 80% of the half-diagonal), in pixels: per corner, the least-squares
    /// shift `d` with `px − scene ≈ d · ∇scene`, averaged over the corners.
    /// (A slow gain, as the colour-shift avoidance applies, does not count.)
    fn corner_shift(raw: &RawData) -> [f32; 2] {
        let Pixels::Bayer(px) = &raw.pixels else { unreachable!() };
        let (cx, cy) = (W as f32 / 2.0, H as f32 / 2.0);
        // Normal equations [sxx, sxy, syy, sxe, sye] per colour and corner.
        let mut n = [[[0f64; 5]; 4]; 2];
        for y in 8..H - 8 {
            for x in 8..W - 8 {
                let c = match (y % 2, x % 2) {
                    (0, 0) => 0,
                    (1, 1) => 1,
                    _ => continue,
                };
                let (fx, fy) = (x as f32, y as f32);
                if (fx - cx).hypot(fy - cy) > 0.8 * cx.hypot(cy) {
                    let s = scene(fx, fy);
                    let gx = ((scene(fx + 0.01, fy) - scene(fx - 0.01, fy)) / 0.02) as f64;
                    let gy = ((scene(fx, fy + 0.01) - scene(fx, fy - 0.01)) / 0.02) as f64;
                    let e = (px[y * W + x] - s) as f64;
                    let q = &mut n[c][usize::from(fx > cx) + 2 * usize::from(fy > cy)];
                    for (v, a) in q.iter_mut().zip([gx * gx, gx * gy, gy * gy, gx * e, gy * e]) {
                        *v += a;
                    }
                }
            }
        }
        n.map(|corners| {
            let len = |[xx, xy, yy, xe, ye]: [f64; 5]| {
                let det = xx * yy - xy * xy;
                ((yy * xe - xy * ye) / det).hypot((xx * ye - xy * xe) / det)
            };
            (corners.into_iter().map(len).sum::<f64>() / 4.0) as f32
        })
    }

    /// Up to ~1.6 px of misregistration in the corners shrinks by at least 70%.
    #[test]
    fn radial_misregistration_shrinks() {
        let raw = mosaic(0.0025, -0.002);
        let before = corner_shift(&raw);
        let after = corner_shift(&corrected(&raw).expect("Bayer"));
        eprintln!("corner misregistration R/B before {before:?} px, after {after:?} px");
        for c in 0..2 {
            assert!(after[c] < 0.3 * before[c], "colour {c}: {} → {}", before[c], after[c]);
        }
    }

    /// Without CA the mosaic is left (nearly) as is.
    #[test]
    fn no_aberration_is_left_alone() {
        let raw = mosaic(0.0, 0.0);
        let out = corrected(&raw).expect("Bayer");
        let (Pixels::Bayer(a), Pixels::Bayer(b)) = (&raw.pixels, &out.pixels) else { unreachable!() };
        let max = a.iter().zip(b).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
        eprintln!("max change {max}");
        assert!(max < 0.01, "max change {max}");
    }

    /// Demosaiced RAWs are not touched.
    #[test]
    fn rgb_is_not_corrected() {
        let raw = RawData { pixels: Pixels::Rgb(vec![[0.5; 3]; W * H]), ..mosaic(0.0, 0.0) };
        assert!(corrected(&raw).is_none());
    }

    /// Time on a real RAW:
    /// `RAWVALLEY_RAW=photo.RW2 cargo test --release -p rawvalley-core ca_on_sample -- --nocapture`.
    #[test]
    fn ca_on_sample() {
        let Ok(path) = std::env::var("RAWVALLEY_RAW") else { return eprintln!("RAWVALLEY_RAW unset, skipped") };
        let raw = crate::raw::read_raw(std::path::Path::new(&path)).unwrap();
        let start = std::time::Instant::now();
        let out = corrected(&raw);
        eprintln!("CA correction of {}×{}: {:?} (applied: {})", raw.width, raw.height, start.elapsed(), out.is_some());
    }
}
