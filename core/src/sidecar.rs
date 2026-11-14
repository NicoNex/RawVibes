//! Non-destructive edits saved next to the photo, `<file>.rawvalley.json`:
//! development, grade, crop and looks.
//!
//! ```json
//! { "format": "rawvalley-sidecar", "version": 1,
//!   "develop": { "wb": [3200.0, 5.0], "exposure": 0.3 },
//!   "params": { … }, "curves": { … },
//!   "crop": { "rect": [0, 0, 1, 1], "angle": 0 }, "ratio": "Free",
//!   "looks": [{ "path": "/luts/Kodak.cube", "intensity": 0.8, "input": "Standard", "on": true }],
//!   "look_blend": "Chain" }
//! ```

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::crop::{Crop, Ratio};
use crate::develop::{Develop, KELVIN};
use crate::grade::{Grade, Params};
use crate::lab::Blend;
use crate::preset::{Curves, grade_from};
use crate::profiles::Profile;

const FORMAT: &str = "rawvalley-sidecar";
const VERSION: u32 = 1;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sidecar {
    pub develop: Develop,
    pub grade: Grade,
    pub crop: Crop,
    pub ratio: Ratio,
    pub looks: Vec<LookRef>,
    pub look_blend: Blend,
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
}

/// Where the sidecar of `photo` lives.
pub fn path_for(photo: &Path) -> PathBuf {
    let mut name = photo.file_name().unwrap_or_default().to_owned();
    name.push(".rawvalley.json");
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
        };
        serde_json::to_string_pretty(&file).expect("sidecars always serialise")
    }

    /// Parses and validates: a sidecar can be edited or come from elsewhere.
    pub fn from_json(text: &str) -> Result<Self, String> {
        let f: File = serde_json::from_str(text).map_err(|e| e.to_string())?;
        if f.format != FORMAT {
            return Err(format!("not a RawValley sidecar ({})", f.format));
        }
        if f.version > VERSION {
            return Err(format!("made by a newer RawValley (sidecar version {})", f.version));
        }
        let d = f.develop;
        let wb_ok = d.wb.is_none_or(|[k, t]| (KELVIN[0]..=KELVIN[1]).contains(&k) && (-150.0..=150.0).contains(&t));
        if !wb_ok || !(-10.0..=10.0).contains(&d.exposure) {
            return Err("invalid development values".into());
        }
        let c = f.crop;
        let crop_ok = c.rect.iter().all(|v| (0.0..=1.0).contains(v)) && (-90.0..=90.0).contains(&c.angle);
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
            develop: Develop { wb: Some([3200.0, 5.0]), exposure: 0.3 },
            grade,
            crop: Crop { rect: [0.1, 0.1, 0.8, 0.7], angle: 2.0 },
            ratio: Ratio::Fixed(3.0, 2.0),
            looks: vec![LookRef { path: "/luts/a.cube".into(), intensity: 0.8, input: Profile::VLog, on: false }],
            look_blend: Blend::Mix,
        }
    }

    #[test]
    fn sidecar_round_trips() {
        let dir = std::env::temp_dir().join(format!("rawvalley-sidecar-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let photo = dir.join("P1.RW2");
        let s = sample();
        s.save(&photo).unwrap();
        assert_eq!(Sidecar::load(&photo).unwrap(), Some(s));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sidecar_path_is_next_to_the_file() {
        assert_eq!(path_for(Path::new("/a/P1.RW2")), PathBuf::from("/a/P1.RW2.rawvalley.json"));
    }

    #[test]
    fn absent_sidecar_is_none() {
        assert_eq!(Sidecar::load(Path::new("/nonexistent/P1.RW2")), Ok(None));
    }

    #[test]
    fn corrupt_or_newer_sidecar_is_an_error() {
        assert!(Sidecar::from_json("{").is_err());
        let newer = sample().to_json().replace("\"version\": 1", "\"version\": 99");
        assert!(Sidecar::from_json(&newer).unwrap_err().contains("newer"));
    }

    #[test]
    fn nan_in_sidecar_is_rejected() {
        let bad = sample().to_json().replace("\"exposure\": 0.3", "\"exposure\": NaN");
        assert!(Sidecar::from_json(&bad).is_err());
        let mut s = sample();
        s.develop.exposure = 50.0;
        assert!(Sidecar::from_json(&s.to_json()).is_err());
    }
}
