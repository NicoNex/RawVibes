//! Audit of every slider: each is swept over its UI range on an image whose
//! right response is known, and must (1) leave the image bit-exact at its
//! neutral value, (2) move its measured quantity the right way at every
//! step (no dead zones, no reversals), (3) for ± sliders, act about as much
//! both ways, (4) stay finite. One run reports every failure at once.

use crate::develop::{Camera, Detail, Develop};
use crate::gpu::Gpu;
use crate::grade::{Grade, Params};
use crate::lab::{oklch_to_srgb, to_oklab};
use crate::noise::NoiseModel;
use crate::photo::Photo;
use crate::profiles::Profile;
use crate::raw::{GpuRaw, Pixels, RawData};
use crate::view::Region;

/// Steps of each sweep.
const STEPS: usize = 11;

/// Which way the measure must move as the slider rises; `Any` = either, but always the same way.
#[derive(Clone, Copy, PartialEq)]
enum Dir {
    Up,
    Down,
    Any,
}

/// Checks a sweep of `m` (measured at `values`, `neutral` among them).
fn judge(name: &str, values: &[f32], m: &[f32], neutral: Option<usize>, dir: Dir, fails: &mut Vec<String>) {
    let line = m.iter().map(|v| format!("{v:.4}")).collect::<Vec<_>>().join(" ");
    eprintln!("{name:<28} {line}");
    if m.iter().any(|v| !v.is_finite()) {
        return fails.push(format!("{name}: non-finite measure"));
    }
    let span = m[m.len() - 1] - m[0];
    if span.abs() < 1e-3 {
        return fails.push(format!("{name}: does nothing over its range (span {span:.5})"));
    }
    let sign = match dir {
        Dir::Up => 1.0,
        Dir::Down => -1.0,
        Dir::Any => span.signum(),
    };
    if sign * span < 0.0 {
        return fails.push(format!("{name}: acts backwards (span {span:.4})"));
    }
    // Each step must move at least a tenth of an average step, the right way.
    let step = span.abs() / (m.len() - 1) as f32;
    for i in 1..m.len() {
        let d = sign * (m[i] - m[i - 1]);
        // Pinned at display white or black: clipping, not a dead zone.
        let clipped = |v: f32| v >= 0.999 || v.abs() <= 1e-4;
        if clipped(m[i]) && clipped(m[i - 1]) {
            continue;
        }
        if d < 0.1 * step {
            fails.push(format!(
                "{name}: {} from {} to {} (step {:.4} of {:.4} expected)",
                if d < 0.0 { "reverses" } else { "dead" },
                values[i - 1],
                values[i],
                m[i] - m[i - 1],
                sign * step
            ));
        }
    }
    // ± sliders: the two halves within a factor 3 of each other.
    if let Some(z) = neutral.filter(|&z| z > 0 && z < m.len() - 1) {
        let (lo, hi) = ((m[z] - m[0]).abs(), (m[m.len() - 1] - m[z]).abs());
        if lo.max(hi) > 3.0 * lo.min(hi) {
            fails.push(format!("{name}: lopsided, {lo:.4} below neutral vs {hi:.4} above"));
        }
    }
}

/// Findings that are the method's own behaviour, not defects: failure prefix, why.
const EXPECTED: [(&str, &str); 12] = [
    ("gain luma: lopsided", "raising gain clips the highlights it is measured on"),
    ("mono mix", "negative filters push weights below zero; the sum is floored, so the far negative end folds"),
    ("vignette midpoint", "past ~60 the vignette starts beyond the corners: nothing left to darken"),
    ("vignette roundness", "shape, not strength: the darkened area need not grow either way"),
    ("vignette feather: lopsided", "a hard edge darkens more area than a soft one widens"),
    ("sharpening threshold (100): dead from 90", "above the noise level every pixel is already protected"),
    ("defringe: dead from 0 to 10", "darktable's safety 20^(1 − strength): weights barely move below strength 0.2"),
    ("defringe: dead from 10 to 20", "as above"),
    ("green defringe: dead from 0 to 10", "the same safety blend as purple"),
    ("green defringe: dead from 10 to 20", "as above"),
    (
        "NR chrominance (change): dead from 90",
        "a 2σ soft threshold has removed the white noise: the last step takes its final ~5%",
    ),
    ("dehaze: lopsided", "dark-channel dehaze on a scene without sky: removing haze has more room than adding it"),
];

fn sweep(lo: f32, hi: f32) -> Vec<f32> {
    (0..STEPS).map(|i| lo + (hi - lo) * i as f32 / (STEPS - 1) as f32).collect()
}

// ------------------------------------------------------------------ grade

const CW: u32 = 128;
const CH: u32 = 64;
const LIGHT: [f32; 3] = [0.35, 0.6, 0.85];
const CHROMA: [f32; 2] = [0.05, 0.1];
/// OKLCh hue centres of the eight bands (`HUES` of grade.wgsl).
const HUES: [f32; 8] = [0.506, 0.960, 1.920, 2.478, 3.403, 4.608, 5.236, 5.725];

/// Rows 0–15: grey ramp 0 → 1. Rows 16–63: per band (16 px columns) six
/// 8-px rows of patches, lightness × chroma.
fn chart() -> Photo {
    let pixels = (0..CW * CH)
        .map(|i| {
            let (x, y) = (i % CW, i / CW);
            let c = if y < 16 {
                [x as f32 / (CW - 1) as f32; 3]
            } else {
                let k = ((y - 16) / 8) as usize;
                oklch_to_srgb(LIGHT[k / 2], CHROMA[k % 2], HUES[(x / 16) as usize])
            };
            [c[0], c[1], c[2], 1.0]
        })
        .collect();
    Photo { width: CW, height: CH, pixels }
}

/// OKLab of each pixel of a render.
fn lab(rgba: &[u8]) -> Vec<[f32; 3]> {
    rgba.as_chunks::<4>().0.iter().map(|p| to_oklab([0, 1, 2].map(|c| p[c] as f32 / 255.0))).collect()
}

fn mean(px: &[[f32; 3]], pick: impl Fn(u32, u32) -> bool, f: impl Fn([f32; 3]) -> f32) -> f32 {
    let (mut s, mut n) = (0.0, 0.0);
    for y in 0..CH {
        for x in 0..CW {
            if pick(x, y) {
                s += f(px[(y * CW + x) as usize]);
                n += 1.0;
            }
        }
    }
    s / n
}

/// Ramp pixels whose input level lies in `lo..hi`.
fn ramp(lo: f32, hi: f32) -> impl Fn(u32, u32) -> bool {
    move |x, y| y < 16 && (lo..hi).contains(&(x as f32 / (CW - 1) as f32))
}

/// The centre 8 × 4 px of the patch of `band` at lightness row `l`, chroma `c` (indices).
fn patch(band: usize, l: usize, c: usize) -> impl Fn(u32, u32) -> bool {
    move |x, y| {
        let (px, py) = (x % 16, y.wrapping_sub(16) % 8);
        y >= 16
            && x / 16 == band as u32
            && ((y - 16) / 8) as usize == l * 2 + c
            && (4..12).contains(&px)
            && (2..6).contains(&py)
    }
}

fn any_patch(l: Option<usize>, c: Option<usize>) -> impl Fn(u32, u32) -> bool {
    move |x, y| {
        let (px, py) = (x % 16, y.wrapping_sub(16) % 8);
        let k = (y.wrapping_sub(16) / 8) as usize;
        y >= 16
            && l.is_none_or(|l| k / 2 == l)
            && c.is_none_or(|c| k % 2 == c)
            && (4..12).contains(&px)
            && (2..6).contains(&py)
    }
}

fn chroma(c: [f32; 3]) -> f32 {
    c[1].hypot(c[2])
}

type Measure = Box<dyn Fn(&[[f32; 3]]) -> f32>;
type Setter = Box<dyn Fn(&mut Params, f32)>;

/// A grade slider: name, setter, UI range, neutral value, measure, direction.
struct Case {
    name: String,
    set: Setter,
    range: (f32, f32),
    neutral: f32,
    base: Box<dyn Fn(&mut Params)>,
    measure: Measure,
    dir: Dir,
}

fn case(
    name: &str,
    set: impl Fn(&mut Params, f32) + 'static,
    range: (f32, f32),
    measure: impl Fn(&[[f32; 3]]) -> f32 + 'static,
    dir: Dir,
) -> Case {
    Case {
        name: name.into(),
        set: Box::new(set),
        range,
        neutral: 0.0,
        base: Box::new(|_| {}),
        measure: Box::new(measure),
        dir,
    }
}

fn grade_cases() -> Vec<Case> {
    let l = |lo, hi| move |px: &[[f32; 3]]| mean(px, ramp(lo, hi), |c| c[0]);
    let grey_a = |px: &[[f32; 3]]| mean(px, ramp(0.3, 0.8), |c| c[1]);
    let grey_b = |px: &[[f32; 3]]| mean(px, ramp(0.3, 0.8), |c| c[2]);
    let sat = |l: Option<usize>, c: Option<usize>| move |px: &[[f32; 3]]| mean(px, any_patch(l, c), chroma);
    let spread = |px: &[[f32; 3]]| mean(px, ramp(0.75, 0.9), |c| c[0]) - mean(px, ramp(0.1, 0.25), |c| c[0]);
    let mut cases = vec![
        case("temperature (b, yellow)", |p, v| p.temperature = v, (-100.0, 100.0), grey_b, Dir::Up),
        case("tint (a, magenta)", |p, v| p.tint = v, (-100.0, 100.0), grey_a, Dir::Up),
        case("exposure", |p, v| p.exposure = v, (-3.0, 3.0), l(0.3, 0.6), Dir::Up),
        case("contrast", |p, v| p.contrast = v, (-100.0, 100.0), spread, Dir::Up),
        case("highlights", |p, v| p.highlights = v, (-100.0, 100.0), l(0.7, 0.9), Dir::Up),
        case("shadows", |p, v| p.shadows = v, (-100.0, 100.0), l(0.05, 0.25), Dir::Up),
        case("whites", |p, v| p.whites = v, (-100.0, 100.0), l(0.85, 1.01), Dir::Up),
        case("blacks", |p, v| p.blacks = v, (-100.0, 100.0), l(0.0, 0.12), Dir::Up),
        case("saturation", |p, v| p.saturation = v, (-100.0, 100.0), sat(None, None), Dir::Up),
        case("vibrance (low chroma)", |p, v| p.vibrance = v, (-100.0, 100.0), sat(None, Some(0)), Dir::Up),
        case("shadow saturation", |p, v| p.sat_shadows = v, (-100.0, 100.0), sat(Some(0), None), Dir::Up),
        case("highlight saturation", |p, v| p.sat_highlights = v, (-100.0, 100.0), sat(Some(2), None), Dir::Up),
        case("subtractive saturation", |p, v| p.film_sat = v, (-100.0, 100.0), sat(Some(1), None), Dir::Any),
        case(
            "colour density (L)",
            |p, v| p.film_density = v,
            (0.0, 100.0),
            move |px: &[[f32; 3]]| mean(px, any_patch(Some(1), Some(1)), |c| c[0]),
            Dir::Down,
        ),
        case("soft light", |p, v| p.soft_light = v, (0.0, 100.0), spread, Dir::Up),
    ];
    // Wheels: luma on its tonal range, chroma x / y on the greys.
    type Wheel = fn(&mut Params) -> &mut [f32; 4];
    let wheels: [(&str, Wheel, (f32, f32)); 4] = [
        ("lift", |p| &mut p.lift, (0.0, 0.2)),
        ("gamma", |p| &mut p.gamma, (0.3, 0.6)),
        ("gain", |p| &mut p.gain, (0.75, 0.95)),
        ("offset", |p| &mut p.offset, (0.3, 0.6)),
    ];
    for (name, w, (lo, hi)) in wheels {
        cases.push(case(&format!("{name} luma"), move |p, v| w(p)[2] = v, (-100.0, 100.0), l(lo, hi), Dir::Any));
        let tone = move |px: &[[f32; 3]], k: usize| mean(px, ramp(lo, hi), |c| c[k]);
        cases.push(case(
            &format!("{name} x (a)"),
            move |p, v| w(p)[0] = v,
            (-1.0, 1.0),
            move |px| tone(px, 1),
            Dir::Any,
        ));
        cases.push(case(
            &format!("{name} y (b)"),
            move |p, v| w(p)[1] = v,
            (-1.0, 1.0),
            move |px| tone(px, 2),
            Dir::Any,
        ));
    }
    let bands = ["red", "orange", "yellow", "green", "aqua", "blue", "purple", "magenta"];
    for (k, band) in bands.into_iter().enumerate() {
        let hue = move |px: &[[f32; 3]]| {
            let (a, b) = (mean(px, patch(k, 1, 1), |c| c[1]), mean(px, patch(k, 1, 1), |c| c[2]));
            // Unwrapped around the band's own hue.
            let d = b.atan2(a) - HUES[k];
            (d + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI
        };
        cases.push(case(&format!("hue {band}"), move |p, v| p.hue_shift[k] = v, (-100.0, 100.0), hue, Dir::Up));
        // The low-chroma patch: the high one reaches the sRGB gamut edge.
        cases.push(case(
            &format!("hue sat {band}"),
            move |p, v| p.hue_sat[k] = v,
            (-100.0, 100.0),
            move |px| mean(px, patch(k, 1, 0), chroma),
            Dir::Up,
        ));
        cases.push(case(
            &format!("hue lum {band}"),
            move |p, v| p.hue_lum[k] = v,
            (-100.0, 100.0),
            move |px| mean(px, patch(k, 1, 1), |c| c[0]),
            Dir::Up,
        ));
    }
    // Mixer: output channel r from input c, measured as that output channel's mean.
    for i in 0..9 {
        let (r, c) = (i / 3, i % 3);
        let out = move |px: &[[f32; 3]]| mean(px, any_patch(Some(1), Some(1)), |lab| crate::lab::from_oklab(lab)[r]);
        cases.push(case(
            &format!("mixer out {r} ← in {c}"),
            move |p, v| p.mixer[i] = v,
            (-200.0, 200.0),
            out,
            Dir::Any,
        ));
    }
    // Mono colour filter: a band of that channel's colour against its complement.
    for (k, (lit, dim)) in [(0, (0, 4)), (1, (3, 7)), (2, (5, 1))] {
        let mut c = case(
            &format!("mono mix {k}"),
            move |p, v| p.mono_mix[k] = v,
            (-200.0, 200.0),
            move |px| mean(px, patch(lit, 1, 1), |c| c[0]) - mean(px, patch(dim, 1, 1), |c| c[0]),
            Dir::Up,
        );
        c.base = Box::new(|p| p.mixer_mono = 1.0);
        cases.push(c);
    }
    // Vignette: corners against the centre, on a flat grey (the ramp would mix in its own gradient).
    let corner = |px: &[[f32; 3]]| mean(px, |x, y| !(8..CW - 8).contains(&x) && !(8..CH - 8).contains(&y), |c| c[0]);
    cases.push(case("vignette amount", |p, v| p.vignette = v, (-100.0, 100.0), corner, Dir::Up));
    for (name, set) in [
        ("vignette midpoint", (|p: &mut Params, v| p.vignette_mid = v) as fn(&mut Params, f32)),
        ("vignette roundness", |p, v| p.vignette_round = v),
        ("vignette feather", |p, v| p.vignette_feather = v),
    ] {
        let whole = |px: &[[f32; 3]]| mean(px, |_, _| true, |c| c[0]);
        let mut c = case(name, set, (-100.0, 100.0), whole, Dir::Any);
        c.base = Box::new(|p| p.vignette = -80.0);
        cases.push(c);
    }
    cases
}

/// Every grade slider over its range, on the chart.
#[test]
fn audit_grade_sliders() {
    let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
    let grey = Photo { width: CW, height: CH, pixels: vec![[0.5, 0.5, 0.5, 1.0]; (CW * CH) as usize] };
    let (chart, grey) = (gpu.upload(&chart()), gpu.upload(&grey));
    let mut fails = Vec::new();
    for c in grade_cases() {
        let photo = if c.name.starts_with("vignette") { &grey } else { &chart };
        let render = |v: f32| {
            let mut g = Grade::default();
            (c.base)(&mut g.params);
            (c.set)(&mut g.params, v);
            gpu.render(photo, &g, None, 0.0)
        };
        let mut base = Grade::default();
        (c.base)(&mut base.params);
        if render(c.neutral) != gpu.render(photo, &base, None, 0.0) {
            fails.push(format!("{}: not the identity at {}", c.name, c.neutral));
        }
        let values = sweep(c.range.0, c.range.1);
        let m: Vec<f32> = values.iter().map(|&v| (c.measure)(&lab(&render(v)))).collect();
        let neutral = values.iter().position(|&v| v == c.neutral);
        judge(&c.name, &values, &m, neutral, c.dir, &mut fails);
    }
    fails.retain(|f| !EXPECTED.iter().any(|(start, _)| f.starts_with(start)));
    assert!(fails.is_empty(), "{} grade slider failures:\n{}", fails.len(), fails.join("\n"));
}

/// Every curve: one point moved over its range, on the chart.
#[test]
fn audit_curves() {
    use crate::grade::{HUE_HUE, HUE_SAT, LUM_SAT, MASTER, neutral_curve};
    let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
    let chart = gpu.upload(&chart());
    let mid = |c: usize| move |px: &[[f32; 3]]| mean(px, ramp(0.4, 0.6), |lab| lab[c]);
    let red = 0usize;
    let red_x = HUES[red] / std::f32::consts::TAU;
    // Curve, the point moved (x, its y is swept around the curve's own value there), measure, direction.
    type CurveCase = (&'static str, usize, f32, Measure, Dir);
    let cases: Vec<CurveCase> = vec![
        ("curve master (mid L)", MASTER, 0.5, Box::new(mid(0)), Dir::Up),
        ("curve red (a)", 1, 0.5, Box::new(mid(1)), Dir::Up),
        ("curve green (a)", 2, 0.5, Box::new(mid(1)), Dir::Down),
        ("curve blue (b)", 3, 0.5, Box::new(mid(2)), Dir::Down),
        (
            "curve hue vs hue (red)",
            HUE_HUE,
            red_x,
            Box::new(move |px: &[[f32; 3]]| {
                let (a, b) = (mean(px, patch(red, 1, 1), |c| c[1]), mean(px, patch(red, 1, 1), |c| c[2]));
                b.atan2(a)
            }),
            Dir::Up,
        ),
        (
            "curve hue vs sat (red)",
            HUE_SAT,
            red_x,
            Box::new(move |px: &[[f32; 3]]| mean(px, patch(red, 1, 1), chroma)),
            Dir::Up,
        ),
        (
            "curve lum vs sat (mid)",
            LUM_SAT,
            0.6,
            Box::new(move |px: &[[f32; 3]]| mean(px, any_patch(Some(1), Some(0)), chroma)),
            Dir::Up,
        ),
    ];
    let mut fails = Vec::new();
    for (name, ch, x, measure, dir) in cases {
        let render = |dy: f32| {
            let mut g = Grade::default();
            let base = if ch == MASTER || ch < HUE_HUE { x } else { 0.5 };
            let mut pts = neutral_curve(ch);
            pts.insert(1, (x, base + dy));
            if crate::grade::is_periodic(ch) {
                // Periodic: the end points are the same hue, both neutral.
                pts[0].1 = 0.5;
            }
            g.curves[ch] = pts;
            gpu.render(&chart, &g, None, 0.0)
        };
        if render(0.0) != gpu.render(&chart, &Grade::default(), None, 0.0) {
            fails.push(format!("{name}: a point on the neutral curve is not the identity"));
        }
        let values = sweep(-0.2, 0.2);
        let m: Vec<f32> = values.iter().map(|&v| measure(&lab(&render(v)))).collect();
        judge(name, &values, &m, Some(STEPS / 2), dir, &mut fails);
        // A curve never folds the ramp back: brighter in, never darker out.
        if ch == MASTER {
            for v in [-0.2, 0.2] {
                let px = lab(&render(v));
                let l: Vec<f32> = (0..CW).map(|x| px[(8 * CW + x) as usize][0]).collect();
                if l.windows(2).any(|w| w[1] < w[0] - 1e-3) {
                    fails.push(format!("{name}: ramp not monotone with the point at {v:+}"));
                }
            }
        }
    }
    assert!(fails.is_empty(), "{} curve failures:\n{}", fails.len(), fails.join("\n"));
}

/// The curve interpolation itself: through every point, never past its
/// neighbours (Fritsch–Carlson), hue curves continuous across 0 / 1.
#[test]
fn curve_interpolation_is_exact_and_monotone() {
    use crate::grade::{CURVE_RES, sample_curve, sample_periodic};
    let at = |s: &[f32], x: f32| s[(x * (CURVE_RES - 1) as f32).round() as usize];
    let mut out = vec![0.0; CURVE_RES];
    for pts in [
        vec![(0.0, 0.0), (0.25, 0.1), (0.5, 0.5), (0.75, 0.95), (1.0, 1.0)],
        vec![(0.0, 0.05), (0.3, 0.3), (0.31, 0.7), (1.0, 0.9)],
        vec![(0.0, 0.0), (0.5, 0.2), (1.0, 1.0)],
    ] {
        sample_curve(&pts, &mut out);
        for &(x, y) in &pts {
            assert!((at(&out, x) - y).abs() < 2e-3, "{pts:?}: misses ({x}, {y}): {}", at(&out, x));
        }
        assert!(out.windows(2).all(|w| w[1] >= w[0] - 1e-6), "{pts:?}: not monotone");
        assert!(out.iter().all(|v| (pts[0].1..=pts[pts.len() - 1].1).contains(v)), "{pts:?}: overshoots");
    }
    let hue = vec![(0.0, 0.5), (0.2, 0.7), (0.6, 0.4), (1.0, 0.5)];
    sample_periodic(&hue, &mut out);
    let (first, last) = (out[0], out[CURVE_RES - 1]);
    assert!((first - last).abs() < 1e-3, "hue curve jumps at 0/1: {first} vs {last}");
    let slope = |a: f32, b: f32| (b - a) * (CURVE_RES - 1) as f32;
    let (s0, s1) = (slope(out[0], out[1]), slope(out[CURVE_RES - 2], out[CURVE_RES - 1]));
    assert!((s0 - s1).abs() < 0.05 * s0.abs().max(0.1), "hue curve slope jumps at 0/1: {s0} vs {s1}");
}

// ------------------------------------------------------------------ RAW develop

const RW: u32 = 192;
const RH: u32 = 128;

fn rgb_raw(gpu: &Gpu, px: Vec<[f32; 3]>, noise: NoiseModel) -> GpuRaw {
    let data = RawData {
        pixels: Pixels::Rgb(px),
        width: RW,
        height: RH,
        pattern: [0; 36],
        pattern_size: [1, 1],
        crop: [0, 0, RW, RH],
        orientation: 1,
        camera: Camera::neutral(),
    };
    let mut raw = gpu.load_raw(&data).unwrap();
    raw.noise = noise;
    raw
}

/// Linear Rec.2020 of the whole RAW developed 1:1.
fn develop(gpu: &Gpu, raw: &GpuRaw, dev: &Develop) -> Vec<[f32; 4]> {
    let r = Region { center: [RW as f32 / 2.0, RH as f32 / 2.0], size: [RW as f32, RH as f32], angle: 0.0 };
    gpu.read_lin(&gpu.develop(raw, &r, RW, RH, dev, Profile::Standard).unwrap())
}

fn luma(p: &[f32; 4]) -> f32 {
    0.2627 * p[0] + 0.678 * p[1] + 0.0593 * p[2]
}

/// Mean and standard deviation of `f` over the pixels `pick` keeps (16 px in from the edges).
fn stats(px: &[[f32; 4]], pick: impl Fn(u32, u32) -> bool, f: impl Fn(&[f32; 4]) -> f32) -> (f32, f32) {
    let v: Vec<f32> = (0..RW * RH)
        .filter(|i| {
            let (x, y) = (i % RW, i / RW);
            (16..RW - 16).contains(&x) && (16..RH - 16).contains(&y) && pick(x, y)
        })
        .map(|i| f(&px[i as usize]))
        .collect();
    let m = v.iter().sum::<f32>() / v.len() as f32;
    (m, (v.iter().map(|x| (x - m).powi(2)).sum::<f32>() / v.len() as f32).sqrt())
}

const NOISE: NoiseModel = NoiseModel { a: 4e-4, b: 4e-6 };

/// Grey 0.2 with Poisson–Gaussian noise of `NOISE`, independent per channel.
fn noisy_flat() -> Vec<[f32; 3]> {
    let mut n = crate::noise::tests::normals(11);
    (0..RW * RH).map(|_| [0; 3].map(|_| 0.2 + (NOISE.a * 0.2 + NOISE.b).sqrt() * n())).collect()
}

/// A soft vertical edge 0.1 → 0.5 at x = RW/2 (logistic, ~2 px wide: what capture
/// sharpening's whole radius range can restore), slightly noisy.
fn edge() -> Vec<[f32; 3]> {
    let mut n = crate::noise::tests::normals(12);
    (0..RW * RH)
        .map(|i| {
            let x = (i % RW) as f32 + 0.5 - RW as f32 / 2.0;
            let v = 0.1 + 0.4 / (1.0 + (-x / 1.2).exp());
            [v + 0.002 * n(); 3]
        })
        .collect()
}

/// A purple fringe two px wide between a dark and a bright grey.
fn fringe(colour: [f32; 3]) -> Vec<[f32; 3]> {
    (0..RW * RH)
        .map(|i| match i % RW {
            x if x < RW / 2 => [0.03; 3],
            x if x < RW / 2 + 2 => colour,
            _ => [0.6; 3],
        })
        .collect()
}

/// Hazy gradient (airlight 0.8, transmission 0.5) with structure.
fn hazy() -> Vec<[f32; 3]> {
    (0..RW * RH)
        .map(|i| {
            let j = 0.05 + 0.45 * (i % RW) as f32 / RW as f32 * (1.0 + 0.3 * ((i / RW) as f32 * 0.7).sin());
            [j * 0.5 + 0.4; 3]
        })
        .collect()
}

/// Deep shadow (−7 EV of the mask) left of x = RW/2, a highlight (−1.3 EV) right: a hard step.
fn zones() -> Vec<[f32; 3]> {
    (0..RW * RH).map(|i| if i % RW < RW / 2 { [0.012; 3] } else { [0.7; 3] }).collect()
}

/// Fine texture on mid grey.
fn texture() -> Vec<[f32; 3]> {
    (0..RW * RH)
        .map(|i| [0.2 * (1.0 + 0.15 * ((i % RW) as f32 * 0.4).sin() * ((i / RW) as f32 * 0.3).cos()); 3])
        .collect()
}

/// `texture` with independent noise per channel, at `NOISE`.
fn noisy_texture() -> Vec<[f32; 3]> {
    let mut n = crate::noise::tests::normals(13);
    texture().into_iter().map(|p| p.map(|v| v + (NOISE.a * v + NOISE.b).sqrt() * n())).collect()
}

type DetailSet = Box<dyn Fn(&mut Develop, f32)>;
type LinMeasure = Box<dyn Fn(&[[f32; 4]]) -> f32>;

/// Every RAW development slider over its range.
#[test]
fn audit_develop_sliders() {
    let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
    let flat = rgb_raw(&gpu, noisy_flat(), NOISE);
    let edge_raw = rgb_raw(&gpu, edge(), NOISE);
    let fringe_raw = rgb_raw(&gpu, fringe([0.35, 0.08, 0.6]), NOISE);
    let green_fringe_raw = rgb_raw(&gpu, fringe([0.12, 0.45, 0.1]), NOISE);
    let hazy_raw = rgb_raw(&gpu, hazy(), NOISE);
    let texture_raw = rgb_raw(&gpu, texture(), NOISE);
    let grey_raw = rgb_raw(&gpu, vec![[0.2; 3]; (RW * RH) as usize], NOISE);
    let noisy_tex = rgb_raw(&gpu, noisy_texture(), NOISE);
    let zones_raw = rgb_raw(&gpu, zones(), NOISE);
    let noisy_plain = develop(&gpu, &noisy_tex, &Develop::default());
    let (np1, np2) = (noisy_plain.clone(), noisy_plain.clone());
    // Mean absolute change from the undeveloped input: how much a tool does.
    fn change(px: &[[f32; 4]], plain: &[[f32; 4]]) -> f32 {
        px.iter().zip(plain).map(|(a, b)| (0..3).map(|c| (a[c] - b[c]).abs()).sum::<f32>()).sum::<f32>()
            / px.len() as f32
    }
    fn all(_: u32, _: u32) -> bool {
        true
    }
    let luma_noise = |px: &[[f32; 4]]| stats(px, all, luma).1;
    let chroma_noise = |px: &[[f32; 4]]| stats(px, all, |p| p[2] - p[0]).1;
    // Acutance: steepest rise across the edge; overshoot: what the bright side passes its level by.
    let acutance = |px: &[[f32; 4]]| {
        (RW / 2 - 4..RW / 2 + 4)
            .map(|x| stats(px, |xx, _| xx == x + 1, luma).0 - stats(px, |xx, _| xx == x, luma).0)
            .fold(0.0, f32::max)
    };
    let halo = |px: &[[f32; 4]]| stats(px, |x, _| (RW / 2 + 1..RW / 2 + 6).contains(&x), luma).0;
    let fringe_chroma =
        |px: &[[f32; 4]]| stats(px, |x, _| (RW / 2..RW / 2 + 2).contains(&x), |p| (p[2] - p[1]).abs()).0;
    let contrast = |px: &[[f32; 4]]| stats(px, all, luma).1;
    let corner = |px: &[[f32; 4]]| {
        let c = |x0: u32, y0: u32| {
            (0..8)
                .flat_map(|y| (0..8).map(move |x| (x0 + x, y0 + y)))
                .map(|(x, y)| luma(&px[(y * RW + x) as usize]))
                .sum::<f32>()
                / 64.0
        };
        c(0, 0) / c(RW / 2 - 4, RH / 2 - 4)
    };
    let mean_luma = |px: &[[f32; 4]]| stats(px, all, luma).0;
    let dark_ev = |px: &[[f32; 4]]| stats(px, |x, _| x < RW / 2 - 20, luma).0.log2();
    let bright_ev = |px: &[[f32; 4]]| stats(px, |x, _| x > RW / 2 + 20, luma).0.log2();
    let warm = |px: &[[f32; 4]]| stats(px, all, |p| p[0] / p[2]).0;
    let magenta = |px: &[[f32; 4]]| stats(px, all, |p| (p[0] + p[2]) / (2.0 * p[1])).0;
    let detail = |f: fn(&mut Detail, f32)| -> DetailSet { Box::new(move |d, v| f(&mut d.detail, v)) };
    let with = |base: Detail, f: fn(&mut Detail, f32)| -> DetailSet {
        Box::new(move |d, v| {
            d.detail = base;
            f(&mut d.detail, v)
        })
    };
    let usm = Detail { usm: 100.0, ..Detail::default() };
    let sharp = Detail { sharpen: 100.0, ..Detail::default() };
    let nr = Detail { nr_luma: 50.0, ..Detail::default() };
    // Name, image, setter, UI range, neutral value (NaN: none), measure, direction.
    type RawCase<'a> = (&'a str, &'a GpuRaw, DetailSet, (f32, f32), f32, LinMeasure, Dir);
    let cases: Vec<RawCase> = vec![
        (
            "exposure (log2)",
            &grey_raw,
            Box::new(|d, v| d.exposure = v),
            (-3.0, 3.0),
            0.0,
            Box::new(move |px| mean_luma(px).log2()),
            Dir::Up,
        ),
        (
            "WB kelvin (R/B)",
            &grey_raw,
            Box::new(|d, v| d.wb = Some([v, 0.0])),
            (2000.0, 15000.0),
            f32::NAN,
            Box::new(warm),
            Dir::Up,
        ),
        (
            "WB tint ((R+B)/2G)",
            &grey_raw,
            Box::new(|d, v| d.wb = Some([5500.0, v])),
            (-100.0, 100.0),
            f32::NAN,
            Box::new(magenta),
            Dir::Up,
        ),
        (
            "NR luminance (change)",
            &noisy_tex,
            detail(|d, v| d.nr_luma = v),
            (0.0, 100.0),
            0.0,
            Box::new(move |px| change(px, &np1)),
            Dir::Up,
        ),
        (
            "NR detail (luma 50)",
            &flat,
            with(nr, |d, v| d.nr_detail = v),
            (0.0, 100.0),
            f32::NAN,
            Box::new(luma_noise),
            Dir::Up,
        ),
        ("NR chrominance", &flat, detail(|d, v| d.nr_chroma = v), (0.0, 100.0), 0.0, Box::new(chroma_noise), Dir::Down),
        (
            "NR chrominance (change)",
            &noisy_tex,
            detail(|d, v| d.nr_chroma = v),
            (0.0, 100.0),
            0.0,
            Box::new(move |px| change(px, &np2)),
            Dir::Up,
        ),
        ("capture sharpening", &edge_raw, detail(|d, v| d.sharpen = v), (0.0, 100.0), 0.0, Box::new(acutance), Dir::Up),
        (
            "capture radius (amount 100)",
            &edge_raw,
            with(sharp, |d, v| d.sharpen_radius = v),
            (0.5, 2.0),
            f32::NAN,
            Box::new(acutance),
            Dir::Up,
        ),
        ("sharpening", &edge_raw, detail(|d, v| d.usm = v), (0.0, 100.0), 0.0, Box::new(halo), Dir::Up),
        (
            "sharpening radius (100)",
            &edge_raw,
            with(usm, |d, v| d.usm_radius = v),
            (0.5, 3.0),
            f32::NAN,
            Box::new(halo),
            Dir::Up,
        ),
        (
            "sharpening threshold (100)",
            &flat,
            with(usm, |d, v| d.usm_threshold = v),
            (0.0, 100.0),
            f32::NAN,
            Box::new(luma_noise),
            Dir::Down,
        ),
        ("defringe", &fringe_raw, detail(|d, v| d.defringe = v), (0.0, 100.0), 0.0, Box::new(fringe_chroma), Dir::Down),
        (
            "green defringe",
            &green_fringe_raw,
            detail(|d, v| d.defringe_green = v),
            (0.0, 100.0),
            0.0,
            Box::new(fringe_chroma),
            Dir::Down,
        ),
        ("shadows (log2)", &zones_raw, detail(|d, v| d.shadows = v), (-100.0, 100.0), 0.0, Box::new(dark_ev), Dir::Up),
        (
            "highlights (log2)",
            &zones_raw,
            detail(|d, v| d.highlights = v),
            (-100.0, 100.0),
            0.0,
            Box::new(bright_ev),
            Dir::Up,
        ),
        ("dehaze", &hazy_raw, detail(|d, v| d.dehaze = v), (-100.0, 100.0), 0.0, Box::new(contrast), Dir::Up),
        (
            "local contrast",
            &texture_raw,
            detail(|d, v| d.local_contrast = v),
            (0.0, 100.0),
            0.0,
            Box::new(contrast),
            Dir::Up,
        ),
        (
            "lens vignetting",
            &grey_raw,
            detail(|d, v| d.lens_vignette = v),
            (-100.0, 100.0),
            0.0,
            Box::new(corner),
            Dir::Up,
        ),
    ];
    let mut fails = Vec::new();
    let plain = Develop::default();
    for (name, raw, set, (lo, hi), neutral, measure, dir) in cases {
        let run = |v: f32| {
            let mut d = plain;
            set(&mut d, v);
            develop(&gpu, raw, &d)
        };
        if !neutral.is_nan() && run(neutral) != develop(&gpu, raw, &plain) {
            fails.push(format!("{name}: not the identity at {neutral}"));
        }
        let values = sweep(lo, hi);
        let m: Vec<f32> = values.iter().map(|&v| measure(&run(v))).collect();
        judge(name, &values, &m, values.iter().position(|&v| v == neutral), dir, &mut fails);
    }
    // NR luminance starts gently: at 1 it does a small fraction of what 100 does.
    let nr_change = |v: f32| {
        let d = Develop { detail: Detail { nr_luma: v, ..Detail::default() }, ..plain };
        change(&develop(&gpu, &noisy_tex, &d), &noisy_plain)
    };
    let (one, five, full) = (nr_change(1.0), nr_change(5.0), nr_change(100.0));
    eprintln!("NR luminance change at 1, 5, 100: {one:.4} {five:.4} {full:.4}");
    if one > 0.1 * full {
        fails.push(format!("NR luminance at 1 already does {:.0}% of 100", 100.0 * one / full));
    }
    // Exposure is exact: each EV doubles the linear signal.
    let at = |ev: f32| mean_luma(&develop(&gpu, &grey_raw, &Develop { exposure: ev, ..plain }));
    for ev in [-2.0, -1.0, 1.0, 2.0] {
        let ratio = at(ev) / at(0.0);
        if (ratio / 2f32.powf(ev) - 1.0).abs() > 0.01 {
            fails.push(format!("exposure {ev} EV: ×{ratio:.4}, expected ×{:.4}", 2f32.powf(ev)));
        }
    }
    // Shadows and highlights act by area and stop at its edge: no halo on the other side.
    let z0 = develop(&gpu, &zones_raw, &plain);
    for (name, d, far, near) in [
        ("shadows +100", Detail { shadows: 100.0, ..Detail::default() }, 0..RW / 2 - 20, RW / 2 + 1..RW / 2 + 8),
        ("highlights −100", Detail { highlights: -100.0, ..Detail::default() }, RW / 2 + 20..RW, RW / 2 - 8..RW / 2),
    ] {
        let z = develop(&gpu, &zones_raw, &Develop { detail: d, ..plain });
        let ev = |r: std::ops::Range<u32>| {
            let m = |px: &[[f32; 4]]| stats(px, |x, _| r.contains(&x), luma).0;
            (m(&z) / m(&z0)).log2()
        };
        let (far, near) = (ev(far), ev(near));
        eprintln!("{name}: target area {far:+.3} EV, other side by the edge {near:+.3} EV");
        if far.abs() < 1.0 || near.abs() > 0.1 * far.abs() {
            fails.push(format!("{name}: target {far:+.3} EV, halo {near:+.3} EV across the edge"));
        }
    }
    fails.retain(|f| !EXPECTED.iter().any(|(start, _)| f.starts_with(start)));
    assert!(fails.is_empty(), "{} develop slider failures:\n{}", fails.len(), fails.join("\n"));
}
