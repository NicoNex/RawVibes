//! RAW development parameters and the camera model they act through:
//! white balance (as shot, Kelvin/tint, picker), RAW exposure, the camera
//! matrix into the linear Rec.2020 working space, and the encode into an
//! input profile. [`develop_pixel`] is the CPU reference the GPU develop
//! pass must reproduce.

use rawler::imgop::matrix::{multiply, multiply_row1, normalize, pseudo_inverse};
use serde::{Deserialize, Serialize};

use crate::photo::{RAW_TO_SCENE, view_transform};
use crate::profiles::{Profile, rec2020_to_rec709, rec2020_to_xyz};

type M3 = [[f32; 3]; 3];

/// What the user set in the Develop group.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Develop {
    /// `None` = as shot; otherwise `[kelvin, tint]`.
    pub wb: Option<[f32; 2]>,
    /// RAW exposure in EV.
    pub exposure: f32,
    /// Spatial detail tools (absent in phase 2 sidecars: neutral).
    #[serde(default)]
    pub detail: Detail,
}

/// The spatial tools of the linear working space. Amounts 0–100 (dehaze
/// −100…100); every amount at 0 leaves the photo bit-exact.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Detail {
    pub nr_luma: f32,
    pub nr_chroma: f32,
    pub nr_detail: f32,
    pub sharpen: f32,
    /// Capture sharpening PSF radius, 0.5–2.0 full-resolution pixels.
    pub sharpen_radius: f32,
    pub defringe: f32,
    pub dehaze: f32,
    pub local_contrast: f32,
    /// Lens vignetting compensation, −100…100.
    pub lens_vignette: f32,
}

impl Detail {
    /// How far, in full-resolution pixels, the active tools read around a
    /// pixel: a zoomed view develops this margin and crops it after.
    pub fn support(&self) -> f32 {
        let d = self;
        let on = |v: f32, px: f32| if v != 0.0 { px } else { 0.0 };
        // NLM patch + search; five à-trous levels; Sobel + reach; dark
        // channel + two guided-filter boxes; six pyramid levels down and up; RL iterations × two blurs.
        on(d.nr_luma, 9.0)
            + on(d.nr_chroma, 62.0)
            + on(d.defringe, 5.0)
            + on(d.dehaze, 67.0)
            + on(d.local_contrast, 256.0)
            // The most iterations at the largest radius, so neither amount nor radius moves the margin (or the caches).
            + on(d.sharpen, 11.0 * 2.0 * 6.0)
    }

    /// Within the sliders' ranges; `None` when a value is not a number.
    pub fn clamped(self) -> Option<Self> {
        let d = self;
        let all = [d.nr_luma, d.nr_chroma, d.nr_detail, d.sharpen, d.sharpen_radius];
        let all = [all.as_slice(), &[d.defringe, d.dehaze, d.local_contrast, d.lens_vignette]].concat();
        if all.iter().any(|v| !v.is_finite()) {
            return None;
        }
        let amount = |v: f32| v.clamp(0.0, 100.0);
        let signed = |v: f32| v.clamp(-100.0, 100.0);
        Some(Self {
            nr_luma: amount(d.nr_luma),
            nr_chroma: amount(d.nr_chroma),
            nr_detail: amount(d.nr_detail),
            sharpen: amount(d.sharpen),
            sharpen_radius: d.sharpen_radius.clamp(0.5, 2.0),
            defringe: amount(d.defringe),
            dehaze: signed(d.dehaze),
            local_contrast: amount(d.local_contrast),
            lens_vignette: signed(d.lens_vignette),
        })
    }
}

impl Default for Detail {
    fn default() -> Self {
        Self {
            nr_luma: 0.0,
            nr_chroma: 0.0,
            nr_detail: 0.0,
            sharpen: 0.0,
            sharpen_radius: 0.8,
            defringe: 0.0,
            dehaze: 0.0,
            local_contrast: 0.0,
            lens_vignette: 0.0,
        }
    }
}

/// White-balance presets: name, Kelvin, tint.
pub const WB_PRESETS: [(&str, f32, f32); 6] = [
    ("Daylight", 5500.0, 0.0),
    ("Cloudy", 6500.0, 10.0),
    ("Shade", 7500.0, 10.0),
    ("Tungsten", 2850.0, 0.0),
    ("Fluorescent", 3800.0, 20.0),
    ("Flash", 5500.0, 0.0),
];

/// Kelvin range of the temperature slider and of the inversion.
pub const KELVIN: [f32; 2] = [2000.0, 15000.0];
/// Tint slider range (±).
pub const TINT: f32 = 100.0;
/// RAW exposure slider range (± EV).
pub const EXPOSURE: f32 = 3.0;
/// One tint unit as a Δuv off the Planckian locus.
const TINT_UV: f32 = 1.0 / 3000.0;

/// A camera's colour model: the as-shot white balance the stored pixels
/// carry, and the matrix from as-shot-balanced camera RGB to linear Rec.2020.
#[derive(Clone, Debug, PartialEq)]
pub struct Camera {
    /// As-shot WB multipliers, G = 1.
    pub as_shot: [f32; 3],
    pub cam2rec2020: M3,
    /// Exposure the camera asks for on top of the raw data (DNG BaselineExposure), in EV.
    pub baseline_ev: f32,
    xyz2cam: M3,
}

impl Camera {
    /// From a D65-adapted XYZ → camera matrix and the as-shot multipliers
    /// (NaN or non-positive ones: neutral).
    pub fn new(xyz2cam: M3, as_shot: [f32; 3], baseline_ev: f32) -> Self {
        let ok = as_shot.iter().all(|v| v.is_finite() && *v > 0.0);
        let as_shot = if ok { as_shot.map(|v| v / as_shot[1]) } else { [1.0; 3] };
        // Rows normalised so camera white maps to RGB white, as rawler does.
        let cam2rec2020 = pseudo_inverse(normalize(multiply(&xyz2cam, &rec2020_to_xyz())));
        Self { as_shot, cam2rec2020, baseline_ev, xyz2cam }
    }

    /// Pixels already in linear Rec.2020 (4-colour sensors, calibrated at load).
    pub fn neutral() -> Self {
        Self::new(pseudo_inverse(rec2020_to_xyz()), [1.0; 3], 0.0)
    }

    /// WB multipliers for `kelvin`/`tint`, relative to as shot (G = 1).
    pub fn gains(&self, kelvin: f32, tint: f32) -> [f32; 3] {
        let cam = multiply_row1(&self.xyz2cam, &xyz(uv_to_xy(locus_uv(kelvin, tint))));
        let m = cam.map(|c| 1.0 / c.max(1e-6));
        std::array::from_fn(|c| m[c] / m[1] / self.as_shot[c])
    }

    /// The Kelvin/tint whose gains are `gains` (inverse of [`Camera::gains`]).
    pub fn kelvin_tint(&self, gains: [f32; 3]) -> [f32; 2] {
        // The neutral those gains make grey, as a camera colour, then as uv.
        let cam: [f32; 3] = std::array::from_fn(|c| 1.0 / (gains[c] * self.as_shot[c]).max(1e-6));
        let target = xy_to_uv(xyz_to_xy(multiply_row1(&pseudo_inverse(self.xyz2cam), &cam)));
        // Nearest locus point, searched in mired (perceptually even).
        let dist = |mired: f32| {
            let p = locus_uv(1e6 / mired, 0.0);
            (p[0] - target[0]).powi(2) + (p[1] - target[1]).powi(2)
        };
        let [lo, hi] = [1e6 / KELVIN[1], 1e6 / KELVIN[0]];
        let mut best = lo;
        for i in 0..=400 {
            let m = lo + (hi - lo) * i as f32 / 400.0;
            if dist(m) < dist(best) {
                best = m;
            }
        }
        let (mut a, mut b) = ((best - 1.0).max(lo), (best + 1.0).min(hi));
        for _ in 0..40 {
            let (m1, m2) = (a + (b - a) / 3.0, b - (b - a) / 3.0);
            if dist(m1) < dist(m2) { b = m2 } else { a = m1 }
        }
        let kelvin = 1e6 / ((a + b) / 2.0);
        let (p, n) = (locus_uv(kelvin, 0.0), locus_normal(kelvin));
        let along = (target[0] - p[0]) * n[0] + (target[1] - p[1]) * n[1];
        // Within the sliders' range, so a picked or restored value shows as it is.
        [kelvin.clamp(KELVIN[0], KELVIN[1]), (along / TINT_UV).clamp(-TINT, TINT)]
    }

    /// The Kelvin/tint of the as-shot white balance.
    pub fn as_shot_kelvin_tint(&self) -> [f32; 2] {
        self.kelvin_tint([1.0; 3])
    }

    /// Kelvin/tint that make `cam` (as-shot-balanced camera RGB) grey;
    /// `None` when a channel is too dark or clipped to trust. Balanced
    /// channels clip at their as-shot gain, not at 1.
    pub fn neutral_for(&self, cam: [f32; 3]) -> Option<[f32; 2]> {
        if (0..3).any(|c| cam[c] < 1e-4 || cam[c] >= 0.98 * self.as_shot[c]) {
            return None;
        }
        Some(self.kelvin_tint(cam.map(|c| cam[1] / c)))
    }

    /// WB gains relative to as shot for `dev`.
    pub fn gains_for(&self, dev: &Develop) -> [f32; 3] {
        dev.wb.map_or([1.0; 3], |[k, t]| self.gains(k, t))
    }
}

/// As-shot-balanced camera RGB → `profile`'s signal: WB, matrix, exposure,
/// gamut, encode. The GPU develop pass mirrors this.
pub fn develop_pixel(cam: [f32; 3], camera: &Camera, dev: &Develop, profile: Profile) -> [f32; 3] {
    let g = camera.gains_for(dev);
    let k = RAW_TO_SCENE * dev.exposure.exp2();
    let wide = multiply_row1(&camera.cam2rec2020, &std::array::from_fn(|c| cam[c] * g[c])).map(|v| v * k);
    if profile.is_log() {
        // Log gamuts are wider than Rec.2020 in practice; the OETF handles the rest.
        multiply_row1(&profile.from_rec2020(), &wide).map(|v| profile.encode(v))
    } else {
        view_transform(gamut_compress(multiply_row1(&rec2020_to_rec709(), &wide)))
    }
}

/// Moves a colour with a negative channel towards its luminance until the
/// smallest channel is 0 (hue kept, nothing clipped per channel); in-gamut
/// colours are returned as they are.
pub fn gamut_compress(rgb: [f32; 3]) -> [f32; 3] {
    let min = rgb[0].min(rgb[1]).min(rgb[2]);
    if min >= 0.0 {
        return rgb;
    }
    let y = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
    if y <= 0.0 {
        return [0.0; 3];
    }
    let t = y / (y - min);
    rgb.map(|v| (y + t * (v - y)).max(0.0))
}

// ------------------------------------------------------------ colorimetry

/// CIE 1960 uv of the Planckian locus at `kelvin` (Kim et al. cubic
/// splines), moved `tint` units along its normal. As with Kelvin, the
/// numbers describe the light being corrected: a greener light (positive
/// tint) is neutralised by more red and blue, so the image turns magenta.
fn locus_uv(kelvin: f32, tint: f32) -> [f32; 2] {
    let t = kelvin.clamp(1667.0, 25000.0) as f64;
    let x = if t <= 4000.0 {
        -0.2661239e9 / t.powi(3) - 0.2343589e6 / t.powi(2) + 0.8776956e3 / t + 0.179910
    } else {
        -3.0258469e9 / t.powi(3) + 2.1070379e6 / t.powi(2) + 0.2226347e3 / t + 0.240390
    };
    let y = if t <= 2222.0 {
        -1.1063814 * x.powi(3) - 1.34811020 * x.powi(2) + 2.18555832 * x - 0.20219683
    } else if t <= 4000.0 {
        -0.9549476 * x.powi(3) - 1.37418593 * x.powi(2) + 2.09137015 * x - 0.16748867
    } else {
        3.0817580 * x.powi(3) - 5.87338670 * x.powi(2) + 3.75112997 * x - 0.37001483
    };
    let uv = xy_to_uv([x as f32, y as f32]);
    if tint == 0.0 {
        return uv;
    }
    let n = locus_normal(kelvin);
    [uv[0] + n[0] * tint * TINT_UV, uv[1] + n[1] * tint * TINT_UV]
}

/// Unit normal of the locus at `kelvin`, pointing towards green.
fn locus_normal(kelvin: f32) -> [f32; 2] {
    let (a, b) = (locus_uv(kelvin * 0.99, 0.0), locus_uv(kelvin * 1.01, 0.0));
    let (du, dv) = (b[0] - a[0], b[1] - a[1]);
    let len = (du * du + dv * dv).sqrt().max(1e-12);
    // Green lies at larger v.
    let n = [-dv / len, du / len];
    if n[1] < 0.0 { [-n[0], -n[1]] } else { n }
}

fn xy_to_uv([x, y]: [f32; 2]) -> [f32; 2] {
    let d = -2.0 * x + 12.0 * y + 3.0;
    [4.0 * x / d, 6.0 * y / d]
}

fn uv_to_xy([u, v]: [f32; 2]) -> [f32; 2] {
    let d = 2.0 * u - 8.0 * v + 4.0;
    [3.0 * u / d, 2.0 * v / d]
}

fn xyz([x, y]: [f32; 2]) -> [f32; 3] {
    [x / y, 1.0, (1.0 - x - y) / y]
}

fn xyz_to_xy(c: [f32; 3]) -> [f32; 2] {
    let s = c[0] + c[1] + c[2];
    [c[0] / s, c[1] / s]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::Profile;

    /// A plausible mirrorless-camera XYZ → camera matrix (D65), and as-shot
    /// multipliers of a daylight shot.
    fn camera() -> Camera {
        let xyz2cam = [[0.7, -0.15, -0.06], [-0.42, 1.24, 0.19], [-0.08, 0.17, 0.62]];
        let as_shot = Camera::new(xyz2cam, [1.0; 3], 0.0).gains(5200.0, 3.0);
        Camera::new(xyz2cam, as_shot, 0.0)
    }

    #[test]
    fn kelvin_tint_round_trips() {
        let cam = camera();
        for t in [2500.0, 3200.0, 4500.0, 5500.0, 6500.0, 9000.0] {
            for tint in [-30.0, 0.0, 30.0] {
                let [k, n] = cam.kelvin_tint(cam.gains(t, tint));
                assert!((k - t).abs() / t < 0.01, "{t} K, tint {tint}: got {k} K");
                assert!((n - tint).abs() < 0.5, "{t} K, tint {tint}: got tint {n}");
            }
        }
    }

    /// Positive tint is magenta: less green, so red and blue gain on green.
    #[test]
    fn positive_tint_is_magenta() {
        let cam = camera();
        let (m, n) = (cam.gains(5500.0, 30.0), cam.gains(5500.0, 0.0));
        assert!(m[0] > n[0] && m[2] > n[2], "{m:?} vs {n:?}");
    }

    #[test]
    fn as_shot_gains_are_one() {
        let cam = camera();
        let [k, t] = cam.as_shot_kelvin_tint();
        let g = cam.gains(k, t);
        assert!(g.iter().all(|v| (v - 1.0).abs() < 1e-3), "{g:?} at {k} K / {t}");
    }

    #[test]
    fn neutral_for_makes_it_grey() {
        let cam = camera();
        // A grey card under 4000 K / +10 light, as the as-shot-balanced sensor sees it.
        let light = cam.gains(4000.0, 10.0);
        let c = light.map(|g| 0.4 / g);
        let [k, t] = cam.neutral_for(c).unwrap();
        assert!((k - 4000.0).abs() < 40.0 && (t - 10.0).abs() < 0.5, "{k} K, tint {t}");
        let g = cam.gains(k, t);
        let grey: [f32; 3] = std::array::from_fn(|i| g[i] * c[i]);
        assert!(grey.iter().all(|v| (v / grey[1] - 1.0).abs() < 1e-3), "{grey:?}");
    }

    #[test]
    fn neutral_for_rejects_clipped_and_black() {
        let cam = camera();
        let clip = cam.as_shot;
        assert_eq!(cam.neutral_for([0.99 * clip[0], 0.5, 0.5]), None);
        assert_eq!(cam.neutral_for([0.0, 0.1, 0.1]), None);
        // Balanced red above 1 but below its clip is fine.
        assert!(clip[0] > 1.05, "fixture: {clip:?}");
        assert!(cam.neutral_for([1.0, 0.5, 0.4]).is_some());
    }

    #[test]
    fn gamut_compress_keeps_in_gamut_and_hue() {
        let inside = [0.2, 0.5, 0.3];
        assert_eq!(gamut_compress(inside), inside);
        let c = [-0.1, 0.5, 0.8];
        let out = gamut_compress(c);
        let y = |c: [f32; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
        assert!(out.iter().copied().fold(f32::MAX, f32::min).abs() < 1e-6, "{out:?}");
        assert!((y(out) - y(c)).abs() < 1e-5);
        // Hue as the direction away from grey in an opponent plane.
        let hue = |c: [f32; 3]| (c[0] - c[1]).atan2(c[0] + c[1] - 2.0 * c[2]).to_degrees();
        assert!((hue(out) - hue(c)).abs() < 2.0, "{} vs {}", hue(out), hue(c));
    }

    /// Phase 1 rendered RAWs as cam ⊙ wb → Rec.709 matrix → × RAW_TO_SCENE →
    /// view transform (frozen here). Rec.2020 must give the same result for
    /// colours Rec.709 can hold.
    #[test]
    fn rec2020_standard_matches_phase1_on_rec709_colours() {
        let cam = camera();
        let xyz2cam = cam.xyz2cam;
        let rgb709_to_xyz = rawler::imgop::xyz::SRGB_TO_XYZ_D65;
        let cam2rgb709 = pseudo_inverse(normalize(multiply(&xyz2cam, &rgb709_to_xyz)));
        for c in [[0.2, 0.3, 0.25], [0.05, 0.1, 0.08], [0.4, 0.35, 0.3], [0.12, 0.2, 0.3]] {
            let phase1 = view_transform(multiply_row1(&cam2rgb709, &c).map(|v| (v * RAW_TO_SCENE).max(0.0)));
            let now = develop_pixel(c, &cam, &Develop::default(), Profile::Standard);
            assert!((0..3).all(|i| (now[i] - phase1[i]).abs() < 2e-4), "{c:?}: {now:?} vs {phase1:?}");
        }
    }
}
