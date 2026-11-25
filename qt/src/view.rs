//! What QML shows, as one JSON document: the editor's state in the words and numbers the controls need.

use rawvibes_core::crop::PRESETS;
use rawvibes_core::develop::{DevPart, KELVIN, WB_PRESETS};
use rawvibes_core::export::PhotoFormat;
use rawvibes_core::grade::{BAND_HUE_RANGE, BAND_HUES, Stage, is_periodic, sample_curve, sample_periodic};
use rawvibes_core::lab::{Blend, oklch_to_srgb};
use rawvibes_core::meta::{self, Exif, Policy, RawMetadata};
use rawvibes_core::profiles::Profile;
use rawvibes_core::sidecar::{Label, Meta};
use serde_json::{Value, json};

use crate::app::command::{DETAILS, PARAMS, off_key};
use crate::app::{App, LabKind, LabLayer, LensStatus, mixer_value_mut, wheel_mut};
use crate::gettext;
use crate::i18n::gettext;

pub fn state(app: &App) -> Value {
    let p = app.grade.params;
    let mut params = serde_json::Map::new();
    for (k, f) in PARAMS {
        params.insert((*k).into(), json!(*f(&mut p.clone())));
    }
    let mut detail = serde_json::Map::new();
    let mut d = app.develop.detail;
    for (k, f) in DETAILS {
        detail.insert((*k).into(), json!(*f(&mut d)));
    }
    let mut defaults = serde_json::Map::new();
    let mut nd = rawvibes_core::develop::Detail::default();
    for (k, f) in DETAILS {
        defaults.insert((*k).into(), json!(*f(&mut nd)));
    }
    let wheels: Vec<Value> = (0..4)
        .map(|i| {
            let w = *wheel_mut(&mut p.clone(), i);
            json!({ "x": w[0], "y": w[1], "luma": w[2] })
        })
        .collect();
    let mixer: Vec<f32> = (0..12).map(|i| *mixer_value_mut(&mut p.clone(), i)).collect();
    let b = app.band;
    let curves: Vec<Value> =
        app.grade.curves.iter().map(|c| Value::Array(c.iter().map(|&(x, y)| json!([x, y])).collect())).collect();
    // Drawn by QML as the shader evaluates them: sampled here, by the core's own interpolation.
    let samples: Vec<Vec<f32>> = app
        .grade
        .curves
        .iter()
        .enumerate()
        .map(|(ch, c)| {
            let mut out = vec![0.0f32; 128];
            if is_periodic(ch) { sample_periodic(c, &mut out) } else { sample_curve(c, &mut out) }
            out.iter().map(|v| (v * 1000.0).round() / 1000.0).collect()
        })
        .collect();
    let roll: Vec<Value> = app
        .roll
        .iter()
        .enumerate()
        .map(|(i, it)| {
            json!({
                "name": crate::app::file_label(&it.path),
                "rating": it.rating,
                "label": label_key(it.label),
                "frame": it.frame,
                "id": it.id,
                "active": app.active == Some(i),
                "shown": app.shows(it.rating, it.label),
            })
        })
        .collect();
    let gallery: Vec<Value> = app
        .luts
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let place = if l.group.is_empty() { String::new() } else { format!("{} · ", l.group) };
            json!({
                "title": l.title,
                "badge": l.format.badge(),
                "tip": format!("{place}{} · {} {}³", l.title, l.format.badge(), l.size),
                "shown": app.gallery_shows(l),
                "thumb": app.ui.thumbs.get(i).copied().unwrap_or_default(),
            })
        })
        .collect();
    let groups: Vec<String> = std::iter::once(gettext("All folders"))
        .chain(app.group_names.iter().map(|g| if g.is_empty() { gettext("Main folder") } else { g.clone() }))
        .collect();
    let group = app.group.as_ref().and_then(|g| app.group_names.iter().position(|n| n == g)).map_or(0, |i| i + 1);
    let size = app.size();
    let camera = app.camera_meta();
    let raw = app.camera().is_some();
    let e = &app.ui.export;
    let policy = app.prefs.policy;
    let defaults = &app.prefs.author;
    json!({
        "tab": app.ui.tab,
        "title": app.path.as_deref().map(crate::app::file_label).unwrap_or_else(|| "RawVibes".into()),
        "info": app.ui.info,
        "photoInfo": photo_info(camera),
        "canvas": app.ui.canvas,
        "photoGen": app.ui.photo_gen,
        "scopesGen": app.ui.scopes_gen,
        "labGen": app.ui.lab_gen,
        "hasSource": app.has_source(),
        "hasPhoto": app.path.is_some(),
        "raw": raw,
        "chart": app.show_chart,
        "compare": app.compare,
        "split": app.split,
        "overlay": app.overlay,
        "skin": app.ui.skin,
        "picker": app.ui.picker,
        "zoomed": app.zoom.is_some(),
        "zoomPercent": app.zoom_percent().round(),
        "undo": app.can_undo(),
        "redo": app.can_redo(),
        "clipboard": app.clipboard.is_some(),
        "rating": app.rating,
        "label": label_key(app.label),
        "labelName": label_name(app.label),
        "labels": Label::ALL.iter().map(|&l| json!({ "key": label_key(l), "name": label_name(l) })).collect::<Vec<_>>(),
        "roll": roll,
        "rollFilter": app.ui.roll_filter,
        "rollFilters": roll_filters(),
        "grid": app.ui.grid,
        // Grade
        "params": params,
        "wheels": wheels,
        "band": b,
        "bandNames": band_names(),
        "bands": band_colours(),
        "hueStrip": hue_strip(),
        "hue": [p.hue_shift[b], p.hue_sat[b], p.hue_lum[b]],
        "curves": curves,
        "samples": samples,
        "mixer": mixer,
        "luma": rawvibes_core::grade::LUMA,
        "mixerMono": p.mixer_mono > 0.5,
        "mixerFree": p.mixer_free > 0.5,
        "matching": app.ui.matching,
        "matchName": app.ui.match_name,
        "matchFile": app.match_file(),
        "matchThumb": app.ui.match_thumb,
        "matchAmount": p.match_amount,
        "off": app.off.iter().map(|&o| off_key(o)).collect::<Vec<_>>(),
        "profile": app.profile,
        "profiles": Profile::ALL.iter().map(|p| json!({ "key": p, "name": p.name() })).collect::<Vec<_>>(),
        "profileIndex": Profile::ALL.iter().position(|&q| q == app.profile).unwrap_or(0),
        "profileName": app.profile.name(),
        "sourceProfileName": app.source_profile().name(),
        "profileHint": profile_hint(app.profile),
        "hdrPreview": app.hdr_preview,
        "headroom": app.headroom,
        // Develop
        "develop": {
            "kelvin": app.kelvin_tint().map_or(5500.0, |kt| kt[0]),
            "tint": app.kelvin_tint().map_or(0.0, |kt| kt[1]),
            "kelvinRange": KELVIN,
            "exposure": app.develop.exposure,
            "wbRow": app.wb_row(),
            "wbPresets": wb_presets(),
            "detail": detail,
            "defaults": defaults,
            "nrAi": app.develop.detail.nr_ai,
            "caCorrect": app.develop.detail.ca_correct,
            "lensTca": app.develop.detail.lens_tca,
            "aiBusy": app.ui.ai_busy,
            "aiFraction": app.ui.ai_fraction,
            "aiMsg": if app.develop.detail.nr_ai { app.ui.ai_msg.clone() } else { None },
            "lensStatus": app.ui.lens_status.as_ref().map(lens_label).unwrap_or_default(),
            "lensFocal": app.ui.lens_focal,
            "lensAperture": app.ui.lens_aperture,
            "lensAsked": app.ui.lens_asked,
            "lensProfile": app.has_lens_profile(),
            "lensResults": app.ui.lens_results.iter().map(|(m, n)| json!({ "maker": m, "model": n })).collect::<Vec<_>>(),
        },
        "quality": crate::app::PreviewQuality::ALL.iter().position(|&q| q == app.quality).unwrap_or(0),
        "refine": app.refine,
        "asInCameraOnOpen": app.prefs.as_in_camera,
        // Crop
        "crop": {
            "rect": app.crop.rect,
            "angle": app.crop.angle,
            "editing": app.editing.is_some(),
            "ratio": app.ratio_row,
            "ratios": PRESETS.iter().map(|(n, _)| gettext(n)).collect::<Vec<_>>(),
            "swappable": matches!(app.ratio, rawvibes_core::crop::Ratio::Fixed(a, b) if a != b),
            "aspect": size.and_then(|(w, h)| app.ratio.aspect(w, h)),
            "frame": size.map_or([0.0; 2], |(w, h)| [w as f32, h as f32]),
        },
        // Looks and LUT Lab
        "looks": layers(&app.looks),
        "lookBlend": app.look_blend,
        "flashLook": app.ui.flash_look,
        "lab": layers(&app.lab_layers),
        "labBlend": app.lab_blend,
        "labInput": app.lab_input,
        "labSummary": summary(&app.lab_layers, app.lab_blend, app.lab_input),
        "blendHint": [chain_hint(), mix_hint()],
        "browsing": app.browsing,
        "gallery": gallery,
        "groups": groups,
        "groupIndex": group,
        "selected": app.selected,
        "candidate": app.candidate.as_ref().map(|c| c.0),
        "lutFolder": app.prefs.lut_folder,
        "presets": app.ui.presets.iter().map(|(n, p)| json!({ "name": n, "path": p })).collect::<Vec<_>>(),
        // Photo export
        "export": {
            "open": e.open,
            "format": e.format.unwrap_or(PhotoFormat::Jpeg).tag(),
            "formats": PhotoFormat::ALL.iter().map(|&f| json!({ "tag": f.tag(), "name": format_label(f), "hdr": f.hdr(), "lossy": f.lossy() })).collect::<Vec<_>>(),
            "quality": e.quality,
            "all": e.all,
            "roll": e.roll,
            "size": gettext!("{} × {} px", e.size.0, e.size.1),
            "headroom": gettext!("{} stops above SDR white · set in the Photo tab", format!("{:.1}", app.headroom)),
            "allSubtitle": gettext!("{} photos, each with its own edits, into a folder", e.roll),
            "estimate": e.estimate,
            "busy": e.busy,
            "progress": e.progress,
            "step": e.step,
        },
        // Metadata
        "meta": meta_view(&app.meta, defaults),
        "camera": camera_card(camera),
        "policyNote": gettext!("On export: {} — {}.", policy_name(policy), policy_hint(policy)),
        "prefs": {
            "policies": Policy::ALL.iter().map(|&p| json!({ "tag": p.tag(), "name": policy_name(p), "hint": policy_hint(p) })).collect::<Vec<_>>(),
            "policy": Policy::ALL.iter().position(|&p| p == policy).unwrap_or(1),
            "suffix": app.prefs.suffix,
            "author": defaults.creator,
            "copyright": defaults.copyright,
            "email": defaults.email,
            "website": defaults.website,
            "background": app.prefs.background,
            "qualities": [gettext("Automatic"), gettext("Fast"), gettext("Balanced"), gettext("Best")],
        },
        "stages": stage_keys(),
    })
}

/// Every eye's key, so QML builds them from one list.
fn stage_keys() -> Value {
    let g: Vec<String> = Stage::ALL.iter().map(|&s| off_key(rawvibes_core::sidecar::Off::Grade(s))).collect();
    let d: Vec<String> = DevPart::ALL.iter().map(|&p| off_key(rawvibes_core::sidecar::Off::Develop(p))).collect();
    json!({ "grade": g, "develop": d })
}

fn layers(list: &[LabLayer]) -> Value {
    Value::Array(
        list.iter()
            .map(|l| {
                let (kind, badge, title) = match &l.kind {
                    LabKind::Lut(lut) => ("lut", lut.format.badge(), lut.title.clone()),
                    LabKind::Grade => ("grade", "GRADE", gettext("Current grade")),
                    LabKind::Convert(_) => ("convert", "CONV", gettext("Conversion")),
                };
                let to = match &l.kind {
                    LabKind::Convert(p) => *p,
                    _ => l.input,
                };
                json!({
                    "kind": kind,
                    "badge": badge,
                    "title": title,
                    "intensity": (l.intensity * 100.0).round(),
                    "input": to,
                    "inputIndex": Profile::ALL.iter().position(|&q| q == to).unwrap_or(0),
                    "on": l.on,
                })
            })
            .collect(),
    )
}

/// Plain-words description of what the exported LUT does.
fn summary(layers: &[LabLayer], blend: Blend, input: Profile) -> String {
    if layers.is_empty() {
        return String::new();
    }
    let name = |l: &LabLayer| match &l.kind {
        LabKind::Lut(lut) => format!("«{}» ({:.0}%)", lut.title, l.intensity * 100.0),
        LabKind::Grade => format!("{} ({:.0}%)", gettext("current grade"), l.intensity * 100.0),
        LabKind::Convert(p) => gettext!("conversion to {}", p.name()),
    };
    let mut steps = vec![gettext!("Input: {}", input.name())];
    let mut at = input;
    match blend {
        Blend::Chain => {
            for l in layers.iter().filter(|l| l.on) {
                if let LabKind::Lut(_) = l.kind
                    && l.input != at
                {
                    steps.push(gettext!("converted to {}", l.input.name()));
                }
                steps.push(name(l));
                at = match l.kind {
                    LabKind::Convert(p) => p,
                    _ => Profile::Standard,
                };
            }
        }
        Blend::Mix => {
            let looks: Vec<String> =
                layers.iter().filter(|l| l.on && !matches!(l.kind, LabKind::Convert(_))).map(name).collect();
            steps.push(gettext!("mix of {}", looks.join(" + ")));
            at = Profile::Standard;
            for l in layers.iter().filter(|l| l.on) {
                if let LabKind::Convert(p) = l.kind {
                    steps.push(name(l));
                    at = p;
                }
            }
        }
    }
    let out = if at.is_log() { at.name().to_string() } else { "Rec.709".into() };
    steps.push(gettext!("Output: {}", out));
    steps.join("  →  ")
}

fn chain_hint() -> String {
    gettext("Chain: layers apply one after another; intensity fades each step.")
}

fn mix_hint() -> String {
    gettext(
        "Mix: every LUT gets the same input; outputs are averaged in OKLab weighted by intensity \
         (below 100% the rest is the neutral image). Conversions apply to the result.",
    )
}

fn band_names() -> [String; 8] {
    [
        gettext("Reds"),
        gettext("Oranges"),
        gettext("Yellows"),
        gettext("Greens"),
        gettext("Teals"),
        gettext("Blues"),
        gettext("Purples"),
        gettext("Magentas"),
    ]
}

/// Swatches and slider tracks in OKLCh from the shader's own band hues: each track shows what moving it does.
fn band_colours() -> Value {
    let hex = |l: f32, c: f32, h: f32| {
        let [r, g, b] = oklch_to_srgb(l, c, h).map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
        format!("#{r:02x}{g:02x}{b:02x}")
    };
    Value::Array(
        BAND_HUES
            .iter()
            .map(|&h| {
                let hue: Vec<String> =
                    [-1.0, -0.5, 0.0, 0.5, 1.0].iter().map(|t| hex(0.72, 0.13, h + t * BAND_HUE_RANGE)).collect();
                json!({
                    "swatch": hex(0.68, 0.15, h),
                    "hue": hue,
                    "sat": [hex(0.72, 0.0, h), hex(0.72, 0.2, h)],
                    "lum": [hex(0.38, 0.1, h), hex(0.92, 0.1, h)],
                })
            })
            .collect(),
    )
}

/// The hue curves' x axis: 72 steps around the OKLCh hue circle, as the shader sees it.
fn hue_strip() -> Vec<String> {
    (0..72)
        .map(|i| {
            let t = (i as f32 + 0.5) / 72.0;
            let [r, g, b] = oklch_to_srgb(0.72, 0.13, t * std::f32::consts::TAU).map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
            format!("#{r:02x}{g:02x}{b:02x}")
        })
        .collect()
}

fn wb_presets() -> Vec<String> {
    std::iter::once(gettext("As shot"))
        // Same order as WB_PRESETS (literal calls, so xgettext finds them).
        .chain([
            gettext("Daylight"),
            gettext("Cloudy"),
            gettext("Shade"),
            gettext("Tungsten"),
            gettext("Fluorescent"),
            gettext("Flash"),
        ])
        .take(WB_PRESETS.len() + 1)
        .chain(std::iter::once(gettext("Custom")))
        .collect()
}

pub fn label_key(l: Label) -> &'static str {
    match l {
        Label::None => "None",
        Label::Red => "Red",
        Label::Yellow => "Yellow",
        Label::Green => "Green",
        Label::Blue => "Blue",
        Label::Purple => "Purple",
    }
}

fn label_name(l: Label) -> String {
    match l {
        Label::None => gettext("No label"),
        Label::Red => gettext("Red"),
        Label::Yellow => gettext("Yellow"),
        Label::Green => gettext("Green"),
        Label::Blue => gettext("Blue"),
        Label::Purple => gettext("Purple"),
    }
}

/// The filmstrip filter's rows: all, at least n stars, then each colour label.
fn roll_filters() -> Vec<String> {
    let mut rows = vec![gettext("All photos")];
    rows.extend((1..=5).map(|n| if n == 5 { "★".repeat(5) } else { format!("{} +", "★".repeat(n)) }));
    rows.extend(Label::ALL.iter().skip(1).map(|&l| label_name(l)));
    rows
}

fn lens_label(status: &LensStatus) -> String {
    match status {
        LensStatus::Auto(name) => gettext!("{} · automatic", name),
        LensStatus::Chosen(name) => gettext!("{} · chosen", name),
        LensStatus::NeedsFocal(name) => gettext!("{}: type in the focal length", name),
        LensStatus::NotFound => gettext("None found"),
        LensStatus::Off => gettext("No profile"),
        LensStatus::NoDatabase => gettext("No lens database: install lensfun"),
    }
}

fn format_label(f: PhotoFormat) -> String {
    match f {
        PhotoFormat::Jpeg => gettext("JPEG · 8-bit, for sharing"),
        PhotoFormat::UltraHdr => gettext("JPEG HDR · gain map, brighter highlights on HDR screens"),
        PhotoFormat::JxlHdr => gettext("JPEG XL HDR · Rec.2100 PQ, small files"),
        PhotoFormat::AvifHdr => gettext("AVIF HDR · Rec.2100 PQ, for the web"),
        PhotoFormat::PngHdr => gettext("PNG HDR · 16-bit Rec.2100 PQ, lossless"),
        PhotoFormat::Png16 => gettext("PNG · 16-bit, lossless"),
        PhotoFormat::Tiff16 => gettext("TIFF · 16-bit, for print and editing"),
    }
}

fn policy_name(p: Policy) -> String {
    match p {
        Policy::All => gettext("All"),
        Policy::Essential => gettext("Essential"),
        Policy::Copyright => gettext("Copyright only"),
        Policy::None => gettext("None"),
    }
}

fn policy_hint(p: Policy) -> String {
    match p {
        Policy::All => gettext("Everything the camera recorded, location and serial numbers included"),
        Policy::Essential => gettext("Camera, exposure, date, rights and description; no location, no serial numbers"),
        Policy::Copyright => gettext("Only the creator, copyright and contact"),
        Policy::None => gettext("No metadata at all"),
    }
}

fn profile_hint(p: Profile) -> String {
    if p.is_log() {
        gettext!(
            "The photo's signal: RAWs are developed into {}, a JPEG or TIFF must already be in it. The exported LUT goes from this log to Rec.709.",
            p.name()
        )
    } else {
        gettext(
            "The photo's signal and the exported LUT's input. Standard: a normal picture (Rec.709), like a camera's standard picture profile.",
        )
    }
}

/// Under the photo: camera · lens · focal length.
fn photo_info(m: Option<&RawMetadata>) -> String {
    let Some(m) = m else { return String::new() };
    let focal = m.exif.focal_length.filter(|r| r.d != 0).map(|r| format!("{} mm", r.as_f32().round()));
    [Some(meta::camera_name(m)), meta::lens_name(m), focal]
        .into_iter()
        .flatten()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

fn meta_view(m: &Meta, defaults: &Meta) -> Value {
    json!({
        "title": m.title,
        "caption": m.caption,
        "keywords": m.keywords,
        "creator": m.creator,
        "copyright": m.copyright,
        "city": m.city,
        "country": m.country,
        "creatorDefault": defaults.creator,
        "copyrightDefault": defaults.copyright,
    })
}

/// The camera's facts of the open photo: headline, exposure, focal length, date, location; dashes when none.
fn camera_card(src: Option<&RawMetadata>) -> Value {
    let e = src.map(|s| &s.exif);
    let camera = src.map(meta::camera_name);
    let lens = e
        .and_then(|e| e.lens_model.clone())
        .or_else(|| src.and_then(|s| s.lens.as_ref()).map(|l| format!("{} {}", l.lens_make, l.lens_model)));
    let headline = [camera, lens].into_iter().flatten().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
    let focal = e.and_then(|e| e.focal_length).filter(|r| r.d != 0).map(|r| format!("{} mm", trim(r.as_f32())));
    let taken = e.and_then(|e| e.date_time_original.as_deref()).map(taken_line);
    let location = e.map(|e| {
        if e.gps.as_ref().is_some_and(|g| g.gps_latitude.is_some()) { gettext("Recorded") } else { gettext("None") }
    });
    let dash = |v: Option<String>| v.filter(|v| !v.is_empty()).unwrap_or_else(|| "—".into());
    json!([dash(Some(headline)), dash(e.map(exposure_line)), dash(focal), dash(taken), dash(location)])
}

/// `2.80` as `2.8`, `8.0` as `8`.
fn trim(v: f32) -> String {
    let s = format!("{v:.1}");
    s.strip_suffix(".0").unwrap_or(&s).to_string()
}

/// `2023:07:14 18:02:11` as `2023-07-14 18:02`.
fn taken_line(d: &str) -> String {
    let (date, time) = d.split_once(' ').unwrap_or((d, ""));
    format!("{} {}", date.replace(':', "-"), time.get(..5).unwrap_or(time)).trim().to_string()
}

/// `1/250 s · f/2.8 · ISO 400 · +0.3 EV`, whatever of it the camera recorded.
fn exposure_line(e: &Exif) -> String {
    let mut parts = Vec::new();
    if let Some(t) = e.exposure_time.filter(|t| t.n != 0 && t.d != 0) {
        let secs = t.as_f32();
        parts.push(if secs < 1.0 { format!("1/{} s", (1.0 / secs).round()) } else { format!("{} s", trim(secs)) });
    }
    // A manual lens reports f/0: no aperture is better than a wrong one.
    if let Some(f) = e.fnumber.filter(|f| f.n != 0 && f.d != 0) {
        parts.push(format!("f/{}", trim(f.as_f32())));
    }
    if let Some(iso) = e.iso_speed_ratings.map(u32::from).or(e.iso_speed) {
        parts.push(format!("ISO {iso}"));
    }
    if let Some(b) = e.exposure_bias.filter(|b| b.d != 0 && b.n != 0) {
        parts.push(format!("{:+.1} EV", b.n as f64 / b.d as f64));
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taken_and_trim() {
        assert_eq!(taken_line("2023:07:14 18:02:11"), "2023-07-14 18:02");
        assert_eq!(trim(2.8), "2.8");
        assert_eq!(trim(8.0), "8");
    }
}
