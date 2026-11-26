//! QML's commands: a name and JSON arguments, turned into the editor's messages.

use std::path::PathBuf;

use serde_json::Value;

use super::*;

fn f(a: &Value, k: &str) -> f64 {
    a.get(k).and_then(Value::as_f64).unwrap_or_default()
}

fn b(a: &Value, k: &str) -> bool {
    a.get(k).and_then(Value::as_bool).unwrap_or_default()
}

fn u(a: &Value, k: &str) -> usize {
    a.get(k).and_then(Value::as_u64).unwrap_or_default() as usize
}

fn s(a: &Value, k: &str) -> String {
    a.get(k).and_then(Value::as_str).unwrap_or_default().to_owned()
}

fn path(a: &Value) -> Option<PathBuf> {
    a.get("path").and_then(Value::as_str).map(PathBuf::from)
}

fn paths(a: &Value) -> Vec<PathBuf> {
    a.get("paths")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(PathBuf::from)
        .collect()
}

/// A serde value of the core (`Profile`, `Blend`, `Label`…) under `k`.
fn typed<T: serde::de::DeserializeOwned>(a: &Value, k: &str) -> Option<T> {
    serde_json::from_value(a.get(k)?.clone()).ok()
}

/// "lab" or "look" (the default), as QML names the two stacks.
fn layers(a: &Value) -> Layers {
    if s(a, "which") == "lab" { Layers::Lab } else { Layers::Look }
}

fn opt_u(a: &Value, k: &str) -> Option<usize> {
    a.get(k).and_then(Value::as_u64).map(|i| i as usize)
}

/// A sidebar section's eye key ("g:Tone", "d:Noise", "looks", "crop") as the core's `Off`.
pub fn off_from_key(key: &str) -> Option<Off> {
    match key.split_once(':') {
        Some(("g", stage)) => serde_json::from_value(Value::String(stage.into())).ok().map(Off::Grade),
        Some(("d", part)) => serde_json::from_value(Value::String(part.into())).ok().map(Off::Develop),
        _ if key == "looks" => Some(Off::Looks),
        _ if key == "crop" => Some(Off::Crop),
        _ => None,
    }
}

pub fn off_key(o: Off) -> String {
    let name = |v: serde_json::Result<Value>| v.ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default();
    match o {
        Off::Grade(s) => format!("g:{}", name(serde_json::to_value(s))),
        Off::Develop(p) => format!("d:{}", name(serde_json::to_value(p))),
        Off::Looks => "looks".into(),
        Off::Crop => "crop".into(),
    }
}

/// The grade's scalar parameters by name, as QML's sliders send them.
pub const PARAMS: &[(&str, Field)] = &[
    ("temperature", |p| &mut p.temperature),
    ("tint", |p| &mut p.tint),
    ("exposure", |p| &mut p.exposure),
    ("contrast", |p| &mut p.contrast),
    ("highlights", |p| &mut p.highlights),
    ("shadows", |p| &mut p.shadows),
    ("whites", |p| &mut p.whites),
    ("blacks", |p| &mut p.blacks),
    ("saturation", |p| &mut p.saturation),
    ("vibrance", |p| &mut p.vibrance),
    ("sat_shadows", |p| &mut p.sat_shadows),
    ("sat_highlights", |p| &mut p.sat_highlights),
    ("film_sat", |p| &mut p.film_sat),
    ("film_density", |p| &mut p.film_density),
    ("soft_light", |p| &mut p.soft_light),
    ("vignette", |p| &mut p.vignette),
    ("vignette_mid", |p| &mut p.vignette_mid),
    ("vignette_round", |p| &mut p.vignette_round),
    ("vignette_feather", |p| &mut p.vignette_feather),
];

/// The development's detail amounts by name.
pub const DETAILS: &[(&str, DetailField)] = &[
    ("nr_luma", |d| &mut d.nr_luma),
    ("nr_chroma", |d| &mut d.nr_chroma),
    ("nr_detail", |d| &mut d.nr_detail),
    ("nr_ai_strength", |d| &mut d.nr_ai_strength),
    ("sharpen", |d| &mut d.sharpen),
    ("sharpen_radius", |d| &mut d.sharpen_radius),
    ("usm", |d| &mut d.usm),
    ("usm_radius", |d| &mut d.usm_radius),
    ("usm_threshold", |d| &mut d.usm_threshold),
    ("dehaze", |d| &mut d.dehaze),
    ("local_contrast", |d| &mut d.local_contrast),
    ("shadows", |d| &mut d.shadows),
    ("highlights", |d| &mut d.highlights),
    ("defringe", |d| &mut d.defringe),
    ("defringe_green", |d| &mut d.defringe_green),
    ("lens_vignette", |d| &mut d.lens_vignette),
    ("lens_vignette_mid", |d| &mut d.lens_vignette_mid),
    ("lens_distortion", |d| &mut d.lens_distortion),
    ("lens_ca_red", |d| &mut d.lens_ca_red),
    ("lens_ca_blue", |d| &mut d.lens_ca_blue),
    ("lens_dist_amount", |d| &mut d.lens_dist_amount),
    ("lens_vig_amount", |d| &mut d.lens_vig_amount),
];

fn lookup<T: Copy>(table: &[(&str, T)], key: &str) -> Option<T> {
    table.iter().find(|(k, _)| *k == key).map(|(_, f)| *f)
}

fn what(a: &Value) -> Option<What> {
    match s(a, "what").as_str() {
        "grade" => Some(What::Grade),
        "identity" => Some(What::Identity),
        "lab" => Some(What::Lab),
        _ => None,
    }
}

fn lut_format(ext: &str) -> Option<Format> {
    Format::from_path(std::path::Path::new(&format!("x.{ext}")))
}

fn meta_field(k: &str) -> Option<MetaField> {
    Some(match k {
        "title" => MetaField::Title,
        "caption" => MetaField::Caption,
        "keywords" => MetaField::Keywords,
        "creator" => MetaField::Creator,
        "copyright" => MetaField::Copyright,
        "city" => MetaField::City,
        "country" => MetaField::Country,
        _ => return None,
    })
}

/// `name` with `a` as a message; `None` for an unknown command or malformed arguments.
pub fn command(name: &str, a: &Value) -> Option<Msg> {
    Some(match name {
        "open" => Msg::Open,
        "rollSelect" => Msg::RollSelect(u(a, "i")),
        "rollRemove" => Msg::RollRemove(u(a, "i")),
        "rollFilter" => Msg::RollFilter(u(a, "f") as u32),
        "grid" => Msg::Grid(b(a, "on")),
        "mergeHdr" => Msg::MergeHdr,
        "set" => Msg::Set(lookup(PARAMS, &s(a, "key"))?, f(a, "v")),
        "wheel" => Msg::Wheel(u(a, "i"), f(a, "x") as f32, f(a, "y") as f32),
        "wheelLuma" => Msg::WheelLuma(u(a, "i"), f(a, "v")),
        "band" => Msg::Band(u(a, "i")),
        "hue" => Msg::Hue(u(a, "k"), f(a, "v")),
        "curve" => {
            let pts: Vec<(f32, f32)> = a
                .get("points")?
                .as_array()?
                .iter()
                .filter_map(|p| Some((p.get(0)?.as_f64()? as f32, p.get(1)?.as_f64()? as f32)))
                .map(|(x, y)| (x.clamp(0.0, 1.0), y.clamp(0.0, 1.0)))
                .collect();
            Msg::Curve(u(a, "ch"), pts)
        }
        "mixer" => Msg::Mixer(u(a, "i"), f(a, "v")),
        "mixerMono" => Msg::MixerMono(b(a, "on")),
        "mixerFree" => Msg::MixerFree(b(a, "on")),
        "resetAll" => Msg::ResetAll,
        "compare" => Msg::Compare(b(a, "on")),
        "split" => Msg::Split(f(a, "x") as f32),
        "overlay" => Msg::Overlay(b(a, "on")),
        "skin" => Msg::SkinLine(b(a, "on")),
        "chart" => Msg::Chart(b(a, "on")),
        "profile" => Msg::Profile(typed(a, "profile")?),
        "off" => Msg::Off(off_from_key(&s(a, "key"))?, b(a, "off")),
        "tab" => Msg::Tab(s(a, "tab")),
        // Develop
        "wbPreset" => Msg::Develop(DevelopEdit::Preset(u(a, "i") as u32)),
        "kelvin" => Msg::Develop(DevelopEdit::Kelvin(f(a, "v") as f32)),
        "tint" => Msg::Develop(DevelopEdit::Tint(f(a, "v") as f32)),
        "rawExposure" => Msg::Develop(DevelopEdit::Exposure(f(a, "v") as f32)),
        "asShot" => Msg::Develop(DevelopEdit::AsShot),
        "cameraExposure" => Msg::Develop(DevelopEdit::CameraExposure),
        "detail" => Msg::Develop(DevelopEdit::Detail(lookup(DETAILS, &s(a, "key"))?, f(a, "v") as f32)),
        "caCorrect" => Msg::Develop(DevelopEdit::CaCorrect(b(a, "on"))),
        "lensTca" => Msg::Develop(DevelopEdit::LensTca(b(a, "on"))),
        "nrAi" => Msg::Develop(DevelopEdit::NrAi(b(a, "on"))),
        "pick" => Msg::Develop(DevelopEdit::Pick(b(a, "on"))),
        "canvasClick" => Msg::CanvasClick([f(a, "x") as f32, f(a, "y") as f32]),
        "asInCamera" => Msg::AsInCamera(CameraPart::All),
        "matchCamera" => Msg::AsInCamera(CameraPart::Match),
        "asInCameraOnOpen" => Msg::AsInCameraOnOpen(b(a, "on")),
        "quality" => Msg::Quality(u(a, "i") as u32),
        "refine" => Msg::Refine(b(a, "on")),
        "lensSearch" => Msg::LensSearch(s(a, "text")),
        "lensPick" => match (a.get("maker").and_then(Value::as_str), a.get("model").and_then(Value::as_str)) {
            (Some(maker), Some(model)) => Msg::LensPick(Some((maker.into(), model.into()))),
            _ => Msg::LensPick(None),
        },
        "lensNone" => Msg::LensPick(Some((String::new(), String::new()))),
        "lensManual" => Msg::LensManual(f(a, "focal") as f32, f(a, "aperture") as f32),
        // Match
        "matchChoose" => Msg::MatchChoose,
        "matchClear" => Msg::MatchClear,
        "matchAmount" => Msg::MatchAmount(f(a, "v")),
        // Crop and view
        "cropEdit" => Msg::CropEdit(b(a, "on")),
        "cropRatio" => Msg::CropRatio(u(a, "i")),
        "cropSwap" => Msg::CropSwap,
        "cropAngle" => Msg::CropAngle(f(a, "v") as f32),
        "cropReset" => Msg::CropReset,
        "cropPress" => Msg::CropPress([f(a, "x") as f32, f(a, "y") as f32], [f(a, "tx") as f32, f(a, "ty") as f32]),
        "cropDrag" => Msg::CropDrag([f(a, "dx") as f32, f(a, "dy") as f32]),
        "level" => Msg::Level(b(a, "on")),
        "cropLevel" => Msg::CropLevel(f(a, "dx") as f32, f(a, "dy") as f32),
        "canvasSize" => Msg::CanvasSize([f(a, "w") as f32, f(a, "h") as f32]),
        "zoom" => Msg::Zoom(match s(a, "cmd").as_str() {
            "fit" => ZoomCmd::Fit,
            "actual" => ZoomCmd::Actual,
            "in" => ZoomCmd::In,
            "out" => ZoomCmd::Out,
            "scroll" => ZoomCmd::Scroll { dy: f(a, "dy"), at: [f(a, "x") as f32, f(a, "y") as f32] },
            "toggle" => ZoomCmd::Toggle([f(a, "x") as f32, f(a, "y") as f32]),
            "pan" => ZoomCmd::Pan { d: [f(a, "dx") as f32, f(a, "dy") as f32], button: u(a, "button") as u32 },
            _ => return None,
        }),
        "escape" => Msg::Escape,
        // History and marks
        "undo" => Msg::Undo,
        "redo" => Msg::Redo,
        "rate" => Msg::Rate(u(a, "n") as u8, b(a, "toggle")),
        "mark" => Msg::Mark(typed(a, "label")?),
        "copyEdits" => Msg::CopyEdits,
        "pasteEdits" => Msg::PasteEdits,
        "pasteEditsAll" => Msg::PasteEditsAll,
        // Dropped files, as Open would add them.
        "rollAdd" => Msg::RollAddPaths(paths(a), None),
        "dialogAnswer" => {
            let answer = DialogAnswer { paths: paths(a), name: s(a, "name"), cancelled: b(a, "cancelled") };
            Msg::DialogAnswer(u(a, "id") as i32, answer)
        }
        "toastAction" => Msg::ToastAction(u(a, "id") as i32),
        // LUTs
        "chooseFolder" => Msg::ChooseFolder,
        "reload" => Msg::Reload,
        "group" => Msg::Group(u(a, "i") as u32),
        "search" => Msg::Search(s(a, "text")),
        "browse" => Msg::Browse(b(a, "on")),
        "selectLut" => Msg::SelectLut(u(a, "i")),
        "hover" => Msg::Hover(opt_u(a, "i")),
        "commit" => Msg::Commit(u(a, "i")),
        "openInLab" => Msg::OpenInLab,
        "layerAdd" => Msg::LayerAdd(
            layers(a),
            match s(a, "kind").as_str() {
                "selected" => LabAdd::Selected,
                "file" => LabAdd::File,
                "grade" => LabAdd::Grade,
                "convert" => LabAdd::Convert,
                _ => return None,
            },
        ),
        "layerRemove" => Msg::LayerRemove(layers(a), u(a, "i")),
        "layerMove" => Msg::LayerMove(layers(a), u(a, "i"), b(a, "up")),
        "layerIntensity" => Msg::LayerIntensity(layers(a), u(a, "i"), f(a, "v")),
        "layerProfile" => Msg::LayerProfile(layers(a), u(a, "i"), typed(a, "profile")?),
        "layerOn" => Msg::LayerOn(layers(a), u(a, "i"), b(a, "on")),
        "labInput" => Msg::LabInput(typed(a, "profile")?),
        "labBlend" => Msg::LabBlend(typed(a, "blend")?),
        "lookBlend" => Msg::LookBlend(typed(a, "blend")?),
        "exportLut" => Msg::Export(lut_format(&s(a, "format"))?, u(a, "size"), what(a)?),
        // Presets
        "presetSave" => Msg::PresetSave,
        "presetApply" => Msg::PresetApply(path(a)?),
        "presetDelete" => Msg::PresetDelete(path(a)?),
        "presetImport" => Msg::PresetImport,
        "presetExport" => Msg::PresetExport,
        // Photo export
        "exportPhoto" => Msg::ExportPhoto(PhotoFormat::from_tag(&s(a, "format"))),
        "exportSettings" => Msg::ExportSettings(
            PhotoFormat::from_tag(&s(a, "format"))?,
            f(a, "quality").clamp(1.0, 100.0) as u8,
            b(a, "all"),
        ),
        "exportGo" => Msg::ExportGo,
        "exportClosed" => Msg::ExportClosed,
        "hdrWhite" => Msg::HdrWhite(b(a, "hdr"), f(a, "v") as f32, b(a, "pq")),
        "hdrPreview" => Msg::HdrPreview(b(a, "on")),
        "headroom" => Msg::Headroom(f(a, "v") as f32),
        // Metadata and preferences
        "meta" => Msg::MetaEdit(meta_field(&s(a, "field"))?, s(a, "text")),
        "background" => Msg::Background(s(a, "name")),
        "metaPolicy" => Msg::MetaPolicy(s(a, "tag")),
        "exportSuffix" => Msg::ExportSuffix(s(a, "text")),
        "authorDefault" => Msg::AuthorDefault(s(a, "key"), s(a, "text")),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eye_keys_round_trip() {
        for o in [Off::Grade(Stage::Tone), Off::Develop(rawvibes_core::develop::DevPart::Noise), Off::Looks, Off::Crop]
        {
            assert_eq!(off_from_key(&off_key(o)), Some(o), "{}", off_key(o));
        }
    }

    #[test]
    fn every_param_and_detail_name_is_unique() {
        for names in [PARAMS.iter().map(|p| p.0).collect::<Vec<_>>(), DETAILS.iter().map(|d| d.0).collect()] {
            let mut sorted = names.clone();
            sorted.sort();
            sorted.dedup();
            assert_eq!(sorted.len(), names.len());
        }
    }

    #[test]
    fn commands_parse() {
        let a = serde_json::json!({ "key": "exposure", "v": 1.5 });
        assert!(matches!(command("set", &a), Some(Msg::Set(_, v)) if v == 1.5));
        assert!(command("set", &serde_json::json!({ "key": "nope" })).is_none());
        assert!(matches!(command("mark", &serde_json::json!({ "label": "Red" })), Some(Msg::Mark(Label::Red))));
        assert!(command("nonsense", &Value::Null).is_none());
    }

    /// Every dialog answers with `paths` (one for single-file and folder dialogs).
    #[test]
    fn dialog_answers_carry_paths() {
        let one = command("dialogAnswer", &serde_json::json!({ "id": 3, "paths": ["/a.cube"] }));
        assert!(matches!(one, Some(Msg::DialogAnswer(3, a)) if a.paths == [PathBuf::from("/a.cube")]));
        let many = command("dialogAnswer", &serde_json::json!({ "id": 4, "paths": ["/a", "/b"] }));
        assert!(matches!(many, Some(Msg::DialogAnswer(4, a)) if a.paths.len() == 2));
        let cancel = command("dialogAnswer", &serde_json::json!({ "id": 5, "cancelled": true }));
        assert!(matches!(cancel, Some(Msg::DialogAnswer(5, a)) if a.cancelled));
    }
}
