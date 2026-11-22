//! The lensfun database (as `tools/lensfun_db.py` converts it): finding a photo's
//! lens and interpolating its calibration at a focal length, aperture and distance,
//! as lensfun 0.3.4 does (database.cpp `FindLenses`, lens.cpp `Interpolate*`).

use std::path::Path;

use serde::Deserialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DistModel {
    None,
    Poly3,
    Poly5,
    Ptlens,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TcaModel {
    None,
    Linear,
    Poly3,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Mount {
    pub name: String,
    pub compat: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Camera {
    pub maker: String,
    pub model: String,
    pub mount: String,
    pub crop: f32,
}

#[derive(Clone, Debug, Deserialize)]
struct DistCal {
    model: DistModel,
    focal: f32,
    k: [f32; 3],
}

#[derive(Clone, Debug, Deserialize)]
struct TcaCal {
    model: TcaModel,
    focal: f32,
    k: [f32; 6],
}

#[derive(Clone, Debug, Deserialize)]
struct VigCal {
    focal: f32,
    aperture: f32,
    distance: f32,
    k: [f32; 3],
}

#[derive(Clone, Debug, Deserialize)]
pub struct Lens {
    pub maker: String,
    pub model: String,
    pub mounts: Vec<String>,
    /// Crop factor of the camera it was calibrated on.
    pub crop: f32,
    pub aspect: f32,
    /// Optical centre offset, normalised as lensfun's.
    pub center: [f32; 2],
    /// Focal range (lensfun's `GuessParameters`).
    pub focal: [f32; 2],
    dist: Vec<DistCal>,
    tca: Vec<TcaCal>,
    vig: Vec<VigCal>,
}

/// A lens's calibration at one focal length, aperture and distance.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Calib {
    pub dist: Option<(DistModel, [f32; 3])>,
    pub tca: Option<(TcaModel, [f32; 6])>,
    pub vig: Option<[f32; 3]>,
}

#[derive(Debug, Deserialize)]
pub struct Db {
    pub mounts: Vec<Mount>,
    pub cameras: Vec<Camera>,
    pub lenses: Vec<Lens>,
}

impl Db {
    pub fn load(path: &Path) -> Result<Db, String> {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        serde_json::from_str(&text).map_err(|e| e.to_string())
    }

    /// The camera `maker` `model` (lensfun's comparison: case and spacing ignored).
    pub fn find_camera(&self, maker: &str, model: &str) -> Option<&Camera> {
        self.cameras.iter().find(|c| same(&c.maker, maker) && same(&c.model, model))
    }

    /// The lens named `maker` `model`, as a sidecar records it.
    pub fn lens(&self, maker: &str, model: &str) -> Option<&Lens> {
        self.lenses.iter().find(|l| same(&l.maker, maker) && same(&l.model, model))
    }

    /// The best match for an EXIF lens name on `camera` (its mount and crop narrow the search):
    /// lensfun's `FindLenses` with every word of the name required.
    // shortcut: the name's own focal/aperture range is not parsed into the pattern (lensfun's
    // GuessParameters on it); the words "16", "35" still have to match. Port it if lenses get mixed up.
    pub fn find_lens(&self, camera: Option<&Camera>, maker: &str, model: &str) -> Option<&Lens> {
        let words = Words::new(model);
        if words.0.is_empty() {
            return None;
        }
        let mounts: Vec<&str> = camera.map(|c| self.compatible(&c.mount)).unwrap_or_default();
        let crop = camera.map_or(0.0, |c| c.crop);
        let mut best: Option<(i32, &Lens)> = None;
        for lens in &self.lenses {
            let score = score(lens, maker, &words, &mounts, crop);
            if score > 0 && best.is_none_or(|(b, _)| score > b) {
                best = Some((score, lens));
            }
        }
        best.map(|(_, l)| l)
    }

    /// `mount` and the mounts it takes lenses of.
    fn compatible<'a>(&'a self, mount: &'a str) -> Vec<&'a str> {
        let mut v = vec![mount];
        if let Some(m) = self.mounts.iter().find(|m| same(&m.name, mount)) {
            v.extend(m.compat.iter().map(String::as_str));
        }
        v
    }

    /// Lenses whose maker and model hold every word of `query`, for picking one by hand.
    pub fn search(&self, query: &str) -> Vec<&Lens> {
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        if words.is_empty() {
            return Vec::new();
        }
        let mut found: Vec<&Lens> = self
            .lenses
            .iter()
            .filter(|l| {
                let name = format!("{} {}", l.maker, l.model).to_lowercase();
                words.iter().all(|w| name.contains(w.as_str()))
            })
            .collect();
        found.sort_by(|a, b| (&a.maker, &a.model).cmp(&(&b.maker, &b.model)));
        found.truncate(200);
        found
    }
}

/// lensfun's `_lf_strcmp` equality: case-insensitive, runs of spaces as one, ends trimmed.
fn same(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    norm(a) == norm(b)
}

/// lensfun's `lfFuzzyStrCmp` word list: digit runs (with dots), punctuation runs and letter
/// runs, lower-cased and sorted; lone punctuation and a lone "f" dropped ("*" and "+" kept).
struct Words(Vec<String>);

impl Words {
    fn new(s: &str) -> Words {
        let chars: Vec<char> = s.chars().collect();
        let mut words = Vec::new();
        let mut i = 0;
        while i < chars.len() {
            if chars[i].is_whitespace() {
                i += 1;
                continue;
            }
            let start = i;
            let c = chars[i];
            i += 1;
            let punct = |c: char| c.is_ascii_punctuation();
            if c.is_ascii_digit() {
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    i += 1;
                }
            } else if punct(c) {
                while i < chars.len() && punct(chars[i]) {
                    i += 1;
                }
            } else {
                while i < chars.len() && !chars[i].is_whitespace() && !chars[i].is_ascii_digit() && !punct(chars[i]) {
                    i += 1;
                }
            }
            if i - start == 1 && (punct(c) || c.eq_ignore_ascii_case(&'f')) && c != '*' && c != '+' {
                continue;
            }
            words.push(chars[start..i].iter().collect::<String>().to_lowercase());
        }
        words.sort();
        Words(words)
    }

    /// lensfun's `Compare` with every pattern word required: 0, or 200·hits / (both lengths).
    fn score(&self, other: &str) -> i32 {
        let m = Words::new(other).0;
        if m.is_empty() {
            return 0;
        }
        let mut mi = 0;
        for p in &self.0 {
            while mi < m.len() && m[mi] < *p {
                mi += 1;
            }
            if mi >= m.len() || m[mi] != *p {
                return 0;
            }
            mi += 1;
        }
        (self.0.len() * 200 / (self.0.len() + m.len())) as i32
    }
}

/// lensfun's `_lf_lens_compare_score` for a pattern of an EXIF name, a maker, the camera's
/// mounts and its crop factor; 0 = no match.
fn score(lens: &Lens, maker: &str, words: &Words, mounts: &[&str], crop: f32) -> i32 {
    let mut score = 0;
    let m = lens.crop;
    if crop > 0.01 && crop < m * 0.96 {
        return 0;
    }
    score += if crop >= m * 1.41 {
        2
    } else if crop >= m * 1.31 {
        4
    } else if crop >= m * 1.21 {
        6
    } else if crop >= m * 1.11 {
        8
    } else if crop >= m * 1.01 {
        10
    } else if crop >= m {
        5
    } else if crop >= m * 0.96 {
        3
    } else {
        0
    };
    // The camera's own mount scores 10, a compatible one 9.
    if !mounts.is_empty() && !lens.mounts.is_empty() {
        let hit = |list: &[&str]| list.iter().any(|a| lens.mounts.iter().any(|b| same(a, b)));
        if hit(&mounts[..1]) {
            score += 10;
        } else if hit(&mounts[1..]) {
            score += 9;
        } else {
            return 0;
        }
    }
    if !maker.trim().is_empty() {
        if !same(maker, &lens.maker) {
            return 0;
        }
        score += 10;
    }
    let s = words.score(&lens.model);
    if s == 0 {
        return 0;
    }
    score + (s * 4 / 10).max(1)
}

/// lensfun's `__insert_spline`: the two calibrations on each side of the wanted focal length.
fn spline<'a, T>(items: impl Iterator<Item = (f32, &'a T)>, focal: f32) -> Result<&'a T, [Option<(f32, &'a T)>; 4]> {
    let mut s: [Option<(f32, &T)>; 4] = [None; 4];
    let mut d = [f32::MIN, f32::MIN, f32::MAX, f32::MAX];
    for (f, item) in items {
        let df = focal - f;
        if df == 0.0 {
            return Ok(item);
        }
        if df < 0.0 {
            if df > d[1] {
                (d[0], s[0]) = (d[1], s[1]);
                (d[1], s[1]) = (df, Some((f, item)));
            } else if df > d[0] {
                (d[0], s[0]) = (df, Some((f, item)));
            }
        } else if df < d[2] {
            (d[3], s[3]) = (d[2], s[2]);
            (d[2], s[2]) = (df, Some((f, item)));
        } else if df < d[3] {
            (d[3], s[3]) = (df, Some((f, item)));
        }
    }
    Err(s)
}

/// lensfun's `_lf_interpolate`: a Hermite spline through y2..y3, tangents from y1/y4 when present.
fn hermite(y1: Option<f32>, y2: f32, y3: f32, y4: Option<f32>, t: f32) -> f32 {
    let tg2 = y1.map_or(y3 - y2, |y1| (y3 - y1) * 0.5);
    let tg3 = y4.map_or(y3 - y2, |y4| (y4 - y2) * 0.5);
    let (t2, t3) = (t * t, t * t * t);
    (2.0 * t3 - 3.0 * t2 + 1.0) * y2 + (t3 - 2.0 * t2 + t) * tg2 + (-2.0 * t3 + 3.0 * t2) * y3 + (t3 - t2) * tg3
}

/// Terms interpolated by focal length as lensfun does: exact focal → that calibration; one side
/// only → the nearest; else the spline, terms scaled by focal (`scaled(i)`) as `__parameter_scales`.
fn interpolate<const N: usize>(
    s: [Option<(f32, &[f32; N])>; 4],
    focal: f32,
    scaled: impl Fn(usize) -> bool,
) -> Option<[f32; N]> {
    let (Some((f1, k1)), Some((f2, k2))) = (s[1], s[2]) else {
        return s[1].or(s[2]).map(|(_, k)| *k);
    };
    let t = (focal - f1) / (f2 - f1);
    Some(std::array::from_fn(|i| {
        // Scaled terms are multiplied by their focal length, the result divided by the wanted one.
        let v = |f: f32, k: &[f32; N]| k[i] * if scaled(i) { f } else { 1.0 };
        let y0 = s[0].map(|(f, k)| v(f, k));
        let y3 = s[3].map(|(f, k)| v(f, k));
        hermite(y0, v(f1, k1), v(f2, k2), y3, t) / if scaled(i) { focal } else { 1.0 }
    }))
}

impl Lens {
    /// The calibration at `focal` mm, f/`aperture`, `distance` m.
    pub fn calib(&self, focal: f32, aperture: f32, distance: f32) -> Calib {
        // Each kind keeps the first model it meets, as lensfun.
        let dist = self.dist.iter().find(|c| c.model != DistModel::None).map(|first| {
            let same: Vec<&DistCal> = self.dist.iter().filter(|c| c.model == first.model).collect();
            let terms = match spline(same.iter().map(|c| (c.focal, &c.k)), focal) {
                Ok(k) => Some(*k),
                // Every distortion term is scaled by focal length (lensfun's __parameter_scales keeps them).
                Err(s) => interpolate(s, focal, |_| true),
            };
            terms.map(|k| (first.model, k))
        });
        let tca = self.tca.iter().find(|c| c.model != TcaModel::None).map(|first| {
            let same: Vec<&TcaCal> = self.tca.iter().filter(|c| c.model == first.model).collect();
            let terms = match spline(same.iter().map(|c| (c.focal, &c.k)), focal) {
                Ok(k) => Some(*k),
                // The first two TCA terms are scale factors: interpolated as they are.
                Err(s) => interpolate(s, focal, |i| i >= 2),
            };
            terms.map(|k| (first.model, k))
        });
        Calib { dist: dist.flatten(), tca: tca.flatten(), vig: self.vignetting(focal, aperture, distance) }
    }

    /// lensfun's `InterpolateVignetting`: inverse distance weighting (power 3.5) over focal,
    /// 4/aperture and 0.1/distance; nothing when the nearest calibration is too far (> 1).
    fn vignetting(&self, focal: f32, aperture: f32, distance: f32) -> Option<[f32; 3]> {
        let [min_f, max_f] = self.focal;
        let span = max_f - min_f;
        let norm = |f: f32| if span != 0.0 { (f - min_f) / span } else { f - min_f };
        let dist = |c: &VigCal| {
            let (df, da, dd) =
                (norm(c.focal) - norm(focal), 4.0 / c.aperture - 4.0 / aperture, 0.1 / c.distance - 0.1 / distance);
            (df * df + da * da + dd * dd).sqrt()
        };
        let (mut terms, mut total, mut nearest) = ([0.0f32; 3], 0.0f32, f32::MAX);
        for c in &self.vig {
            let d = dist(c);
            if d < 0.0001 {
                return Some(c.k);
            }
            nearest = nearest.min(d);
            let w = (1.0 / d.powf(3.5)).abs();
            (0..3).for_each(|i| terms[i] += w * c.k[i]);
            total += w;
        }
        (nearest <= 1.0 && total > 0.0).then(|| terms.map(|t| t / total))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Db {
        Db::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/lensfun-mini.json")).unwrap()
    }

    #[derive(Deserialize)]
    struct Ref {
        camera: String,
        lens: String,
        focal: f32,
        aperture: f32,
        distance: f32,
        dist: RefDist,
        tca: RefTca,
        vig: [f32; 3],
    }
    #[derive(Deserialize)]
    struct RefDist {
        model: String,
        k: [f32; 3],
    }
    #[derive(Deserialize)]
    struct RefTca {
        model: String,
        k: [f32; 6],
    }

    fn refs() -> Vec<Ref> {
        let text =
            std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/lensfun-ref.json")).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    #[test]
    fn finds_lens_by_exif_name() {
        let db = db();
        let cam = db.find_camera("canon", "Canon EOS  5D Mark III");
        assert!(cam.is_some());
        let lens = db
            .find_lens(cam, "", "EF16-35mm f/2.8L USM")
            .or_else(|| db.find_lens(cam, "", "Canon EF 16-35mm f/2.8L USM"));
        assert_eq!(lens.map(|l| l.model.as_str()), Some("Canon EF 16-35mm f/2.8L USM"));
        let l = db.find_lens(db.find_camera("Panasonic", "DC-S5"), "", "LUMIX S 20-60/F3.5-5.6");
        assert_eq!(l.map(|l| l.model.as_str()), Some("Lumix S 20-60/F3.5-5.6"));
    }

    #[test]
    fn unknown_lens_has_no_profile() {
        let db = db();
        assert!(db.find_lens(db.find_camera("Panasonic", "DC-S5"), "", "Helios 44-2 58mm f/2").is_none());
        assert!(db.find_lens(None, "", "").is_none());
    }

    #[test]
    fn interpolation_matches_lensfun() {
        let db = db();
        for r in refs() {
            let lens = db.lenses.iter().find(|l| l.model == r.lens).unwrap();
            let c = lens.calib(r.focal, r.aperture, r.distance);
            let close = |a: &[f32], b: &[f32]| a.iter().zip(b).all(|(x, y)| (x - y).abs() <= 1e-4 * (1.0 + y.abs()));
            let (dm, dk) = c.dist.unwrap();
            assert_eq!(format!("{dm:?}").to_lowercase(), r.dist.model, "{} {}", r.camera, r.lens);
            assert!(close(&dk, &r.dist.k), "{} @{}: {dk:?} vs {:?}", r.lens, r.focal, r.dist.k);
            let (tm, tk) = c.tca.unwrap();
            assert_eq!(format!("{tm:?}").to_lowercase(), r.tca.model);
            assert!(close(&tk, &r.tca.k), "{} @{}: {tk:?} vs {:?}", r.lens, r.focal, r.tca.k);
            let vig = c.vig.unwrap_or([0.0; 3]);
            assert!(
                close(&vig, &r.vig),
                "{} @{} f/{} {}m: {vig:?} vs {:?}",
                r.lens,
                r.focal,
                r.aperture,
                r.distance,
                r.vig
            );
        }
    }

    #[test]
    fn search_finds_by_words() {
        let db = db();
        let found = db.search("16-35 2.8");
        assert_eq!(found.first().map(|l| l.model.as_str()), Some("Canon EF 16-35mm f/2.8L USM"));
        assert!(db.search("lumix").iter().all(|l| l.model.contains("Lumix")));
        assert!(db.search("zzz").is_empty());
    }
}
