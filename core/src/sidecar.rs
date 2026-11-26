//! Non-destructive edits saved next to the photo, `<file>.rawvibes.json`:
//! development, grade, crop, looks, star rating, colour label and metadata.
//!
//! ```json
//! { "format": "rawvibes-sidecar", "version": 2,
//!   "develop": { "wb": [3200.0, 5.0], "exposure": 0.3, "detail": { "nr_luma": 20.0, … } },
//!   "params": { … }, "curves": { … },
//!   "crop": { "rect": [0, 0, 1, 1], "angle": 0 }, "ratio": "Free",
//!   "looks": [{ "path": "/luts/Kodak.cube", "intensity": 0.8, "input": "Standard", "on": true }],
//!   "look_blend": "Chain", "rating": 4, "label": "Green",
//!   "meta": { "title": "Alba", "keywords": ["mare"], "creator": "", … } }
//! ```

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::crop::{Crop, Ratio};
use crate::develop::{DevPart, Develop, EXPOSURE, KELVIN, TINT};
use crate::grade::{Grade, Params, Stage};
use crate::lab::Blend;
use crate::preset::{Curves, grade_from};
use crate::profiles::Profile;

const FORMAT: &str = "rawvibes-sidecar";
const VERSION: u32 = 2;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sidecar {
    pub develop: Develop,
    pub grade: Grade,
    pub crop: Crop,
    pub ratio: Ratio,
    pub looks: Vec<LookRef>,
    pub look_blend: Blend,
    /// Stars, 0 (unrated) to 5.
    pub rating: u8,
    pub label: Label,
    /// The input profile the photo was graded in; `None` in older sidecars.
    pub profile: Option<Profile>,
    /// Sections switched off with their eye toggle.
    pub off: Vec<Off>,
    pub meta: Meta,
    /// The lens profile chosen by hand; `None` = found from the EXIF.
    pub lens_profile: Option<LensChoice>,
}

/// A lens profile picked in the Lens section, with the focal length and aperture a manual lens
/// does not record. Empty maker and model: no profile at all.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LensChoice {
    pub maker: String,
    pub model: String,
    pub focal: f32,
    pub aperture: f32,
}

/// The photo's own metadata, written into its exports; empty fields take the Preferences' defaults.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Meta {
    pub title: String,
    pub caption: String,
    pub keywords: Vec<String>,
    pub creator: String,
    pub copyright: String,
    pub email: String,
    pub website: String,
    pub city: String,
    pub country: String,
}

/// A sidebar section switched off with its eye toggle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Off {
    Grade(Stage),
    Develop(DevPart),
    /// The LUTs applied after the grade.
    Looks,
    Crop,
}

/// Colour label of a photo, for sorting a shoot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Label {
    #[default]
    None,
    Red,
    Yellow,
    Green,
    Blue,
    Purple,
}

impl Label {
    pub const ALL: [Label; 6] = [Label::None, Label::Red, Label::Yellow, Label::Green, Label::Blue, Label::Purple];
}

/// A look LUT by file, with its settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LookRef {
    pub path: PathBuf,
    pub intensity: f32,
    pub input: Profile,
    pub on: bool,
}

#[derive(Serialize, Deserialize)]
struct File {
    format: String,
    version: u32,
    #[serde(default)]
    develop: Develop,
    #[serde(default)]
    params: Params,
    #[serde(default)]
    curves: Curves,
    #[serde(default)]
    crop: Crop,
    #[serde(default)]
    ratio: Ratio,
    #[serde(default)]
    looks: Vec<LookRef>,
    #[serde(default)]
    look_blend: Blend,
    #[serde(default)]
    rating: u8,
    #[serde(default)]
    label: Label,
    #[serde(default)]
    profile: Option<Profile>,
    #[serde(default)]
    off: Vec<Off>,
    #[serde(default)]
    meta: Meta,
    #[serde(default)]
    lens_profile: Option<LensChoice>,
}

/// Where the sidecar of `photo` lives.
pub fn path_for(photo: &Path) -> PathBuf {
    let mut name = photo.file_name().unwrap_or_default().to_owned();
    name.push(".rawvibes.json");
    photo.with_file_name(name)
}

impl Sidecar {
    /// The sidecar of `photo`; `Ok(None)` when there is none.
    pub fn load(photo: &Path) -> Result<Option<Self>, String> {
        match std::fs::read_to_string(path_for(photo)) {
            Ok(text) => Self::from_json(&text).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    /// Writes the sidecar of `photo` (through a temporary file, so a crash
    /// never leaves half a sidecar).
    pub fn save(&self, photo: &Path) -> Result<(), String> {
        let path = path_for(photo);
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, self.to_json()).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
    }

    pub fn to_json(&self) -> String {
        let file = File {
            format: FORMAT.into(),
            version: VERSION,
            develop: self.develop,
            params: self.grade.params,
            curves: Curves::from(self.grade.curves.clone()),
            crop: self.crop,
            ratio: self.ratio,
            looks: self.looks.clone(),
            look_blend: self.look_blend,
            rating: self.rating,
            label: self.label,
            profile: self.profile,
            off: self.off.clone(),
            meta: self.meta.clone(),
            lens_profile: self.lens_profile.clone(),
        };
        serde_json::to_string_pretty(&file).expect("sidecars always serialise")
    }

    /// Parses and validates: a sidecar can be edited or come from elsewhere.
    pub fn from_json(text: &str) -> Result<Self, String> {
        let f: File = serde_json::from_str(text).map_err(|e| e.to_string())?;
        if f.format != FORMAT {
            return Err(format!("not a RawVibes sidecar ({})", f.format));
        }
        if f.version > VERSION {
            return Err(format!("made by a newer RawVibes (sidecar version {})", f.version));
        }
        // Out-of-range development values are brought into the sliders' range
        // (a whole sidecar is not worth losing over them); non-numbers are not.
        let d = f.develop;
        let detail = d.detail.clamped();
        if !d.exposure.is_finite() || d.wb.is_some_and(|wb| wb.iter().any(|v| !v.is_finite())) || detail.is_none() {
            return Err("invalid development values".into());
        }
        let d = Develop {
            wb: d.wb.map(|[k, t]| [k.clamp(KELVIN[0], KELVIN[1]), t.clamp(-TINT, TINT)]),
            exposure: d.exposure.clamp(-EXPOSURE, EXPOSURE),
            detail: detail.expect("checked above"),
        };
        let c = f.crop;
        let [_, _, w, h] = c.rect;
        // Rotated, the rect is in the turned frame and may leave [0, 1]: only the
        // image's size can judge it, so the editor fits it in on load (`crop::clamp`).
        let crop_ok =
            c.rect.iter().all(|v| v.is_finite()) && w >= 0.01 && h >= 0.01 && (-90.0..=90.0).contains(&c.angle);
        let ratio_ok = match f.ratio {
            Ratio::Fixed(a, b) => a > 0.0 && b > 0.0 && a.is_finite() && b.is_finite(),
            _ => true,
        };
        if !crop_ok || !ratio_ok {
            return Err("invalid crop".into());
        }
        if f.looks.iter().any(|l| !(0.0..=1.0).contains(&l.intensity)) {
            return Err("invalid look intensity".into());
        }
        Ok(Self {
            develop: d,
            grade: grade_from(f.params, f.curves)?,
            crop: c,
            ratio: f.ratio,
            looks: f.looks,
            look_blend: f.look_blend,
            profile: f.profile,
            off: f.off,
            meta: f.meta,
            lens_profile: f.lens_profile,
            rating: f.rating.min(5),
            label: f.label,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Sidecar {
        let mut grade = Grade::default();
        grade.params.exposure = 0.4;
        Sidecar {
            develop: Develop {
                wb: Some([3200.0, 5.0]),
                exposure: 0.3,
                detail: crate::develop::Detail { nr_luma: 20.0, dehaze: -15.0, ..Default::default() },
            },
            grade,
            crop: Crop { rect: [0.1, 0.1, 0.8, 0.7], angle: 2.0 },
            ratio: Ratio::Fixed(3.0, 2.0),
            looks: vec![LookRef { path: "/luts/a.cube".into(), intensity: 0.8, input: Profile::VLog, on: false }],
            look_blend: Blend::Mix,
            rating: 4,
            label: Label::Green,
            profile: Some(Profile::VLog),
            off: vec![Off::Grade(Stage::Tone), Off::Develop(DevPart::Noise), Off::Looks, Off::Crop],
            meta: Meta { title: "Alba".into(), keywords: vec!["mare".into(), "luce".into()], ..Default::default() },
            lens_profile: Some(LensChoice {
                maker: "Zenit".into(),
                model: "Helios 44-2".into(),
                focal: 58.0,
                aperture: 2.0,
            }),
        }
    }

    #[test]
    fn sidecar_round_trips() {
        let dir = std::env::temp_dir().join(format!("rawvibes-sidecar-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let photo = dir.join("P1.RW2");
        let s = sample();
        s.save(&photo).unwrap();
        assert_eq!(Sidecar::load(&photo).unwrap(), Some(s));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Sidecars from before AI denoise keep classic noise reduction.
    #[test]
    fn sidecar_without_ai_fields_is_classic() {
        let d: crate::develop::Detail = serde_json::from_str(r#"{ "nr_luma": 20.0 }"#).unwrap();
        assert!(!d.nr_ai);
        assert_eq!(d.nr_ai_strength, 100.0);
    }

    #[test]
    fn sidecar_path_is_next_to_the_file() {
        assert_eq!(path_for(Path::new("/a/P1.RW2")), PathBuf::from("/a/P1.RW2.rawvibes.json"));
    }

    #[test]
    fn absent_sidecar_is_none() {
        assert_eq!(Sidecar::load(Path::new("/nonexistent/P1.RW2")), Ok(None));
    }

    #[test]
    fn corrupt_or_newer_sidecar_is_an_error() {
        assert!(Sidecar::from_json("{").is_err());
        let newer = sample().to_json().replace("\"version\": 2", "\"version\": 99");
        assert!(Sidecar::from_json(&newer).unwrap_err().contains("newer"));
    }

    #[test]
    fn nan_in_sidecar_is_rejected() {
        let bad = sample().to_json().replace("\"exposure\": 0.3", "\"exposure\": NaN");
        assert!(Sidecar::from_json(&bad).is_err());
    }

    /// Values the sliders cannot show are brought into range, not rejected.
    #[test]
    fn out_of_range_development_is_clamped() {
        let mut s = sample();
        s.develop = Develop { wb: Some([30000.0, 300.0]), exposure: 50.0, ..Default::default() };
        s.develop.detail.dehaze = -400.0;
        let back = Sidecar::from_json(&s.to_json()).unwrap();
        assert_eq!(back.develop.wb, Some([KELVIN[1], TINT]));
        assert_eq!((back.develop.exposure, back.develop.detail.dehaze), (EXPOSURE, -100.0));
    }

    /// Phase 2 sidecars (version 1) have no detail tools: they load neutral.
    #[test]
    fn sidecar_v1_loads_with_neutral_detail() {
        let v1 = r#"{ "format": "rawvibes-sidecar", "version": 1,
            "develop": { "wb": [3200.0, 5.0], "exposure": 0.3 } }"#;
        let s = Sidecar::from_json(v1).unwrap();
        assert_eq!(s.develop.detail, crate::develop::Detail::default());
        assert_eq!(s.develop.exposure, 0.3);
        assert_eq!(s.meta, Meta::default());
        // No lens choice: automatic; manual corrections neutral, profile amounts full.
        assert_eq!(s.lens_profile, None);
        let d = s.develop.detail;
        assert_eq!((d.lens_distortion, d.lens_ca_red, d.lens_dist_amount, d.lens_tca), (0.0, 0.0, 100.0, true));
    }

    #[test]
    fn degenerate_crops_are_rejected() {
        for rect in [[0.5, 0.5, 0.0, 0.0], [f32::NAN, 0.0, 0.5, 1.0]] {
            let mut s = sample();
            s.crop.rect = rect;
            assert!(Sidecar::from_json(&s.to_json()).is_err(), "{rect:?}");
        }
    }
    /// A rect off [0, 1] (rounding, or a rotated crop by an edge) loaded no edits at all.
    #[test]
    fn crops_off_the_unit_square_load() {
        for rect in [[-1.9370997e-7, 0.18197213, 1.0, 0.5538462], [-0.01, 0.1, 0.05, 0.05]] {
            let mut s = sample();
            s.crop = Crop { rect, angle: 5.0 };
            assert_eq!(Sidecar::from_json(&s.to_json()).expect("loads").crop.rect, rect);
        }
    }
}
