//! Lens corrections: lensfun 0.3.4's models (modifier.cpp normalisation, mod-coord.cpp
//! distortion and auto-scale, mod-subpix.cpp TCA, mod-color.cpp vignetting) for a lens's
//! interpolated calibration, plus the manual corrections, as the plain numbers the RAW
//! develop shader applies per output pixel. `Correction::source` and `Correction::gain` are
//! the CPU reference of exactly what the shader does.

use crate::develop::Detail;
use crate::lensdb::{Calib, DistModel, Lens, TcaModel};

/// What the shader needs to correct one photo; `Default` changes nothing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Correction {
    /// Sensor pixels → lensfun's normalised coordinates (`NormScale`), and the optical centre there.
    pub norm: f32,
    pub center: [f32; 2],
    /// The auto-scale zoom that hides the empty corners distortion correction leaves (1 = none).
    pub scale: f32,
    /// Manual distortion: r_src = r·(1 + k·r²).
    pub manual_k: f32,
    /// Profile distortion: model (0 none, 1 poly3, 2 poly5, 3 ptlens) and terms, scaled by the amount.
    pub dist_model: u32,
    pub dist: [f32; 3],
    /// Profile TCA: model (0 none, 1 linear, 2 poly3) and terms (vr, vb, cr, cb, br, bb).
    pub tca_model: u32,
    pub tca: [f32; 6],
    /// Manual CA: red and blue radial scales.
    pub ca: [f32; 2],
    /// Profile vignetting (pa) terms, scaled by the amount, and 1 / lensfun's `AspectRatioCorrection`.
    pub vig: [f32; 3],
    pub vig_norm: f32,
    /// Manual vignetting amount (−1…1) and its radius scale (2^(−midpoint/50)).
    pub manual_vig: f32,
    pub manual_mid: f32,
    /// Sensor size in pixels, for the manual vignetting radius.
    pub size: [f32; 2],
}

impl Default for Correction {
    fn default() -> Self {
        Self {
            norm: 1.0,
            center: [0.0; 2],
            scale: 1.0,
            manual_k: 0.0,
            dist_model: 0,
            dist: [0.0; 3],
            tca_model: 0,
            tca: [1.0, 1.0, 0.0, 0.0, 0.0, 0.0],
            ca: [1.0; 2],
            vig: [0.0; 3],
            vig_norm: 1.0,
            manual_vig: 0.0,
            manual_mid: 1.0,
            size: [1.0; 2],
        }
    }
}

/// A photo's lens profile: what of the lens and its calibration the correction needs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Profile {
    /// Crop factor of the calibration camera, the lens's aspect ratio and optical centre offset.
    pub crop: f32,
    pub aspect: f32,
    pub center: [f32; 2],
    /// Crop factor of the camera the photo was taken with.
    pub camera_crop: f32,
    pub calib: Calib,
}

impl Profile {
    /// `lens` at `calib` on a camera of `camera_crop` (the lens's own when the camera is unknown).
    pub fn new(lens: &Lens, calib: Calib, camera_crop: Option<f32>) -> Profile {
        let camera_crop = camera_crop.filter(|c| *c > 0.0).unwrap_or(lens.crop);
        Profile { crop: lens.crop, aspect: lens.aspect, center: lens.center, camera_crop, calib }
    }
}

/// A lens's calibration for a photo; `None` without a focal length (a manual lens nobody told
/// us about: no guessing). Unknown aperture means no vignetting; unknown distance is far (1000 m).
pub fn calib_at(lens: &Lens, focal: f32, aperture: f32, distance: f32) -> Option<Calib> {
    if focal <= 0.0 {
        return None;
    }
    let distance = if distance > 0.0 { distance } else { 1000.0 };
    let c = lens.calib(focal, aperture.max(f32::MIN_POSITIVE), distance);
    Some(Calib { vig: c.vig.filter(|_| aperture > 0.0), ..c })
}

/// The correction of a `sensor`-sized RAW: its lens `profile` and the settings in `d`.
pub fn correction(profile: Option<&Profile>, d: &Detail, sensor: (u32, u32)) -> Correction {
    // lfModifier's constructor: sizes measured between pixel centres, r = 1 at half the short
    // side scaled from the calibration sensor to this one.
    let (wf, hf) = ((sensor.0.max(2) - 1) as f32, (sensor.1.max(2) - 1) as f32);
    let size = wf.min(hf);
    let image_aspect = wf.max(hf) / size;
    // Without a profile the manual corrections use lensfun's defaults (3:2, crop 1).
    let (cal_crop, aspect, lc, crop) =
        profile.map_or((1.0, 1.5, [0.0; 2], 1.0), |p| (p.crop, p.aspect, p.center, p.camera_crop));
    let aspect_corr = (aspect * aspect + 1.0).sqrt();
    let coord = 1.0 / (image_aspect * image_aspect + 1.0).sqrt() * cal_crop / crop * aspect_corr;
    let calib = profile.map(|p| p.calib).unwrap_or_default();
    let (dist_model, dist) = match calib.dist {
        // At amount 0 there is no distortion to correct (nor any auto-scale margin to add).
        Some((m, k)) if d.lens_dist_amount > 0.0 => {
            let id = match m {
                DistModel::None => 0,
                DistModel::Poly3 => 1,
                DistModel::Poly5 => 2,
                DistModel::Ptlens => 3,
            };
            (id, k.map(|t| t * d.lens_dist_amount / 100.0))
        }
        _ => (0, [0.0; 3]),
    };
    let (tca_model, tca) = match calib.tca.filter(|_| d.lens_tca) {
        Some((TcaModel::Linear, k)) => (1, k),
        Some((TcaModel::Poly3, k)) => (2, k),
        _ => (0, Correction::default().tca),
    };
    let ca = [1.0 + 0.002 * d.lens_ca_red / 100.0, 1.0 + 0.002 * d.lens_ca_blue / 100.0];
    let mut c = Correction {
        norm: 2.0 / size * coord,
        center: [(wf / size + lc[0]) * coord, (hf / size + lc[1]) * coord],
        scale: 1.0,
        manual_k: -0.25 * d.lens_distortion / 100.0,
        dist_model,
        dist,
        tca_model,
        tca,
        ca,
        vig: calib.vig.unwrap_or_default().map(|t| t * d.lens_vig_amount / 100.0),
        vig_norm: 1.0 / aspect_corr,
        manual_vig: d.lens_vignette / 100.0,
        manual_mid: (-d.lens_vignette_mid / 50.0).exp2(),
        size: [sensor.0 as f32, sensor.1 as f32],
    };
    c.scale = c.autoscale(wf, hf);
    c
}

impl Correction {
    /// The sensor pixel channel `channel` (0 R, 1 G, 2 B) samples for corrected-image pixel `p`.
    pub fn source(&self, p: [f32; 2], channel: usize) -> [f32; 2] {
        let [cx, cy] = self.center;
        let (x, y) = ((p[0] * self.norm - cx) / self.scale, (p[1] * self.norm - cy) / self.scale);
        let (mut x, mut y) = self.warp(x, y);
        // Lateral CA (mod-subpix.cpp): the red and blue planes scale radially; green is the reference.
        if channel != 1 {
            let b = usize::from(channel == 2);
            let k = &self.tca;
            let f = match self.tca_model {
                1 => k[b],
                2 => {
                    let r2 = x * x + y * y;
                    k[4 + b] * r2 + k[2 + b] * r2.sqrt() + k[b]
                }
                _ => 1.0,
            } * self.ca[b];
            (x, y) = (x * f, y * f);
        }
        [(x + cx) / self.norm, (y + cy) / self.norm]
    }

    /// The vignetting gain at sensor pixel `s`: the profile's (lensfun's de-vignetting `1 / (1 +
    /// k1·r² + k2·r⁴ + k3·r⁶)`, r = 1 at half the diagonal) times the manual one.
    pub fn gain(&self, s: [f32; 2]) -> f32 {
        let x = (s[0] * self.norm - self.center[0]) * self.vig_norm;
        let y = (s[1] * self.norm - self.center[1]) * self.vig_norm;
        let r2 = x * x + y * y;
        let [k1, k2, k3] = self.vig;
        let profile = 1.0 / (1.0 + k1 * r2 + k2 * r2 * r2 + k3 * r2 * r2 * r2);
        // The manual gain RawVibes always had: 1 + a·r²(1 + r²), r = 1 at the sensor's corners.
        // `s` is a pixel centre: the sensor's middle is at (size − 1) / 2.
        let half = [self.size[0] / 2.0, self.size[1] / 2.0];
        let (dx, dy) = (s[0] + 0.5 - half[0], s[1] + 0.5 - half[1]);
        let r2 = (dx * dx + dy * dy) / (half[0] * half[0] + half[1] * half[1]) * self.manual_mid;
        let v = self.manual_vig.abs() * r2 * (1.0 + r2);
        profile * if self.manual_vig >= 0.0 { 1.0 + v } else { 1.0 / (1.0 + v) }
    }

    /// The coordinate corrections of every channel (normalised): manual, then the profile's distortion.
    fn warp(&self, x: f32, y: f32) -> (f32, f32) {
        let r2 = x * x + y * y;
        let m = 1.0 + self.manual_k * r2;
        let (x, y) = (x * m, y * m);
        let ru2 = x * x + y * y;
        let [a, b, c] = self.dist;
        let f = match self.dist_model {
            1 => 1.0 - a + a * ru2,
            2 => 1.0 + a * ru2 + b * ru2 * ru2,
            3 => {
                let r = ru2.sqrt();
                a * ru2 * r + b * ru2 + c * r + 1.0 - a - b - c
            }
            _ => 1.0,
        };
        (x * f, y * f)
    }

    /// lensfun's `GetAutoScale`: the zoom at which the corrected image's border points, moved by
    /// the coordinate corrections, all land inside the sensor.
    fn autoscale(&self, wf: f32, hf: f32) -> f32 {
        let subpixel = if self.tca_model != 0 || self.ca != [1.0; 2] { 1.001 } else { 1.0 };
        if self.dist_model == 0 && self.manual_k == 0.0 {
            return subpixel;
        }
        let (max_x, max_y) = (wf / 2.0 * self.norm, hf / 2.0 * self.norm);
        let corner = hf.atan2(wf);
        let diag = (wf * wf + hf * hf).sqrt() * 0.5 * self.norm;
        let pi = std::f32::consts::PI;
        let points = [
            (0.0, wf * 0.5 * self.norm),
            (corner, diag),
            (pi / 2.0, hf * 0.5 * self.norm),
            (pi - corner, diag),
            (pi, wf * 0.5 * self.norm),
            (pi + corner, diag),
            (pi * 1.5, hf * 0.5 * self.norm),
            (2.0 * pi - corner, diag),
        ];
        let residual = |x: f32, y: f32| (x - max_x).max(-max_x - x).max(y - max_y).max(-max_y - y);
        // Newton's method for the radius whose corrected point lands on the sensor's edge.
        let transformed = |angle: f32, dist: f32| -> f32 {
            let (sa, ca) = (angle as f64).sin_cos();
            let (sa, ca) = (sa as f32, ca as f32);
            let at = |ru: f32| {
                let (x, y) = self.warp(ca * ru, sa * ru);
                residual(x, y) as f64
            };
            let (mut ru, mut dx) = (dist, 0.0001f32);
            for countdown in (0..=50).rev() {
                let rd = at(ru);
                if rd > -0.001 && rd < 0.001 {
                    break;
                }
                if countdown == 0 {
                    return -1.0;
                }
                let rd1 = at(ru + dx);
                if (rd1 - rd).abs() < 0.00001 {
                    dx *= 2.0;
                    continue;
                }
                ru -= (rd / ((rd1 - rd) / dx as f64)) as f32;
            }
            ru
        };
        let scale = points.iter().map(|&(a, d)| d / transformed(a, d)).fold(0.01f32, f32::max);
        scale * 1.001 * subpixel
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lensdb::Db;
    use serde::Deserialize;
    use std::path::Path;

    #[derive(Deserialize)]
    struct Point {
        p: [f32; 2],
        src: [f32; 6],
        gain: f32,
    }
    #[derive(Deserialize)]
    struct Ref {
        lens: String,
        camera_crop: f32,
        w: u32,
        h: u32,
        focal: f32,
        aperture: f32,
        distance: f32,
        autoscale: f32,
        points: Vec<Point>,
    }

    fn data(name: &str) -> String {
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data").join(name)).unwrap()
    }

    /// Every reference case: the lens, its calibration, and lensfun's results.
    fn cases() -> Vec<(Lens, Calib, Ref)> {
        let db: Db = serde_json::from_str(&data("lensfun-mini.json")).unwrap();
        let refs: Vec<Ref> = serde_json::from_str(&data("lensfun-ref.json")).unwrap();
        refs.into_iter()
            .map(|r| {
                let lens = db.lenses.iter().find(|l| l.model == r.lens).unwrap().clone();
                let calib = calib_at(&lens, r.focal, r.aperture, r.distance).unwrap();
                (lens, calib, r)
            })
            .collect()
    }

    #[test]
    fn source_matches_lensfun() {
        for (lens, calib, r) in cases() {
            // lensfun's reference ran without the auto-scale.
            let c = Correction {
                scale: 1.0,
                ..correction(Some(&Profile::new(&lens, calib, Some(r.camera_crop))), &Detail::default(), (r.w, r.h))
            };
            for pt in &r.points {
                for ch in 0..3 {
                    let s = c.source(pt.p, ch);
                    let want = [pt.src[2 * ch], pt.src[2 * ch + 1]];
                    let err = (s[0] - want[0]).hypot(s[1] - want[1]) * c.norm;
                    assert!(err < 1e-4, "{} {}x{} {:?} ch{ch}: {s:?} vs {want:?}", r.lens, r.w, r.h, pt.p);
                }
            }
        }
    }

    #[test]
    fn gain_matches_lensfun() {
        for (lens, calib, r) in cases() {
            let c = correction(Some(&Profile::new(&lens, calib, Some(r.camera_crop))), &Detail::default(), (r.w, r.h));
            for pt in &r.points {
                let g = c.gain(pt.p);
                assert!((g - pt.gain).abs() <= 1e-4 * pt.gain, "{} {:?}: {g} vs {}", r.lens, pt.p, pt.gain);
            }
        }
    }

    #[test]
    fn autoscale_matches_lensfun() {
        for (lens, calib, r) in cases() {
            let c = correction(Some(&Profile::new(&lens, calib, Some(r.camera_crop))), &Detail::default(), (r.w, r.h));
            assert!(
                (c.scale - r.autoscale).abs() <= 1e-4 * r.autoscale,
                "{} {}x{}: {} vs {}",
                r.lens,
                r.w,
                r.h,
                c.scale,
                r.autoscale
            );
        }
    }

    #[test]
    fn profile_needs_a_focal_length() {
        let (lens, _, _) = cases().remove(0);
        assert!(calib_at(&lens, 0.0, 4.0, 10.0).is_none());
        let no_aperture = calib_at(&lens, 20.0, 0.0, 0.0).unwrap();
        assert!(no_aperture.vig.is_none() && no_aperture.dist.is_some());
    }

    #[test]
    fn unknown_camera_uses_the_lens_crop() {
        for (lens, calib, r) in cases() {
            let d = Detail::default();
            let a = correction(Some(&Profile::new(&lens, calib, None)), &d, (r.w, r.h));
            let b = correction(Some(&Profile::new(&lens, calib, Some(lens.crop))), &d, (r.w, r.h));
            assert_eq!(a, b, "{}", r.lens);
        }
    }

    #[test]
    fn portrait_matches_landscape() {
        let (lens, calib, _) = cases().remove(0);
        let d = Detail::default();
        let wide = correction(Some(&Profile::new(&lens, calib, Some(1.0))), &d, (6000, 4000));
        let tall = correction(Some(&Profile::new(&lens, calib, Some(1.0))), &d, (4000, 6000));
        for p in [[0.0, 0.0], [1500.0, 3000.0], [5999.0, 10.0]] {
            let (a, b) = (wide.source(p, 0), tall.source([p[1], p[0]], 0));
            assert!((a[0] - b[1]).abs() < 1e-2 && (a[1] - b[0]).abs() < 1e-2, "{p:?}: {a:?} {b:?}");
        }
    }

    #[test]
    fn identity_is_identity() {
        let c = Correction::default();
        for p in [[0.0, 0.0], [123.5, 77.25]] {
            assert!((0..3).all(|ch| c.source(p, ch) == p));
            assert_eq!(c.gain(p), 1.0);
        }
        let none = correction(None, &Detail::default(), (6000, 4000));
        assert!(
            (0..3).all(|ch| none.source([10.0, 20.0], ch).iter().zip([10.0, 20.0]).all(|(a, b)| (a - b).abs() < 1e-3))
        );
        assert_eq!(none.scale, 1.0);
    }

    #[test]
    fn manual_distortion_sign() {
        // Positive distortion corrects barrel: a corner's source moves towards the centre.
        let d = Detail { lens_distortion: 50.0, ..Detail::default() };
        let c = Correction { scale: 1.0, ..correction(None, &d, (6000, 4000)) };
        let s = c.source([0.0, 0.0], 1);
        assert!(s[0] > 0.0 && s[1] > 0.0, "{s:?}");
        // And the auto-scale keeps the frame full.
        assert!(correction(None, &Detail { lens_distortion: -50.0, ..d }, (6000, 4000)).scale > 1.0);
    }

    #[test]
    fn amount_zero_is_identity() {
        for (lens, calib, r) in cases() {
            let d = Detail { lens_dist_amount: 0.0, lens_vig_amount: 0.0, lens_tca: false, ..Detail::default() };
            let c = correction(Some(&Profile::new(&lens, calib, Some(r.camera_crop))), &d, (r.w, r.h));
            for pt in &r.points {
                assert!(
                    (0..3).all(|ch| c.source(pt.p, ch).iter().zip(pt.p).all(|(a, b)| (a - b).abs() < 1e-2)),
                    "{}",
                    r.lens
                );
                assert!((c.gain(pt.p) - 1.0).abs() < 1e-6);
            }
        }
    }
}
