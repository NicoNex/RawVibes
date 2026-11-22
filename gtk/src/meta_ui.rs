//! The Metadata tab: what the camera recorded (read only) and the photo's own
//! title, keywords, rights and location, saved in its sidecar.

use relm4::adw::prelude::*;
use relm4::{ComponentSender, adw, gtk};

use rawvibes_core::meta::{Exif, RawMetadata};
use rawvibes_core::sidecar::Meta;

use crate::{App, Msg, gettext, section, vbox};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetaField {
    Title,
    Caption,
    Keywords,
    Creator,
    Copyright,
    City,
    Country,
}

pub struct MetaUi {
    pub root: gtk::Box,
    /// Camera, lens, exposure, focal length, date, location.
    camera: [adw::ActionRow; 6],
    fields: Vec<(MetaField, adw::EntryRow, String)>,
}

fn list() -> gtk::ListBox {
    gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build()
}

impl MetaUi {
    pub fn new(sender: &ComponentSender<App>) -> Self {
        let root = vbox(12);
        let cam_list = list();
        let camera = [
            gettext("Camera"),
            gettext("Lens"),
            gettext("Exposure"),
            gettext("Focal length"),
            gettext("Taken"),
            gettext("Location"),
        ]
        .map(|t| {
            let row = adw::ActionRow::builder().title(t).subtitle("—").css_classes(["property"]).build();
            row.set_subtitle_selectable(true);
            cam_list.append(&row);
            row
        });
        root.append(&section(&gettext("Camera"), &cam_list));
        let mut fields = Vec::new();
        for (heading, group) in [
            (gettext("Description"), &[(MetaField::Title, gettext("Title")), (MetaField::Caption, gettext("Caption")), (MetaField::Keywords, gettext("Keywords, comma separated"))][..]),
            (gettext("Rights"), &[(MetaField::Creator, gettext("Creator")), (MetaField::Copyright, gettext("Copyright"))]),
            (gettext("Location"), &[(MetaField::City, gettext("City")), (MetaField::Country, gettext("Country"))]),
        ] {
            let l = list();
            for (f, title) in group {
                let row = adw::EntryRow::builder().title(title.as_str()).build();
                let (snd, f) = (sender.clone(), *f);
                row.connect_changed(move |r| snd.input(Msg::MetaEdit(f, r.text().into())));
                l.append(&row);
                fields.push((f, row, title.clone()));
            }
            root.append(&section(&heading, &l));
        }
        Self { root, camera, fields }
    }

    /// The camera's facts of the open photo; dashes when it recorded none.
    pub fn show_camera(&self, src: Option<&RawMetadata>) {
        let e = src.map(|s| &s.exif);
        let camera = src.map(|s| {
            if s.model.starts_with(&s.make) { s.model.clone() } else { format!("{} {}", s.make, s.model).trim().to_string() }
        });
        let lens = e.and_then(|e| e.lens_model.clone()).or_else(|| src.and_then(|s| s.lens.as_ref()).map(|l| format!("{} {}", l.lens_make, l.lens_model)));
        let focal = e.and_then(|e| e.focal_length).filter(|r| r.d != 0).map(|r| format!("{} mm", trim(r.n as f64 / r.d as f64)));
        let taken = e.and_then(|e| e.date_time_original.as_deref()).map(taken_line);
        let location = e.map(|e| if e.gps.as_ref().is_some_and(|g| g.gps_latitude.is_some()) { gettext("Recorded") } else { gettext("None") });
        for (row, v) in self.camera.iter().zip([camera, lens, e.map(exposure_line), focal, taken, location]) {
            row.set_subtitle(v.filter(|v| !v.is_empty()).as_deref().unwrap_or("—"));
        }
    }

    /// Shows `m`; Creator and Copyright name the Preferences' default they fall back to.
    pub fn set(&self, m: &Meta, defaults: &Meta) {
        for (f, row, title) in &self.fields {
            let text = match f {
                MetaField::Keywords => m.keywords.join(", "),
                _ => field(m, *f).clone(),
            };
            if row.text() != text {
                row.set_text(&text);
            }
            let fallback = match f {
                MetaField::Creator => &defaults.creator,
                MetaField::Copyright => &defaults.copyright,
                _ => "",
            };
            row.set_title(&if fallback.is_empty() { title.clone() } else { gettext!("{} (empty: {})", title, fallback) });
        }
    }
}

fn field(m: &Meta, f: MetaField) -> &String {
    match f {
        MetaField::Title => &m.title,
        MetaField::Caption => &m.caption,
        MetaField::Creator => &m.creator,
        MetaField::Copyright => &m.copyright,
        MetaField::City => &m.city,
        MetaField::Country => &m.country,
        MetaField::Keywords => unreachable!("a list"),
    }
}

/// Puts `text` in `f` of `m`; false when it already held it (an echo of `set`).
pub fn apply(m: &mut Meta, f: MetaField, text: &str) -> bool {
    if f == MetaField::Keywords {
        let k = parse_keywords(text);
        return k != m.keywords && {
            m.keywords = k;
            true
        };
    }
    let s = match f {
        MetaField::Title => &mut m.title,
        MetaField::Caption => &mut m.caption,
        MetaField::Creator => &mut m.creator,
        MetaField::Copyright => &mut m.copyright,
        MetaField::City => &mut m.city,
        MetaField::Country => &mut m.country,
        MetaField::Keywords => unreachable!("handled above"),
    };
    *s != text && {
        *s = text.to_string();
        true
    }
}

/// Keywords typed comma-separated, trimmed, empty ones dropped.
pub fn parse_keywords(text: &str) -> Vec<String> {
    text.split(',').map(str::trim).filter(|k| !k.is_empty()).map(String::from).collect()
}

/// `2.80` as `2.8`, `8.0` as `8`.
fn trim(v: f64) -> String {
    let s = format!("{v:.1}");
    s.strip_suffix(".0").unwrap_or(&s).to_string()
}

/// `2023:07:14 18:02:11` as `2023-07-14 18:02`.
fn taken_line(d: &str) -> String {
    let (date, time) = d.split_once(' ').unwrap_or((d, ""));
    format!("{} {}", date.replace(':', "-"), time.get(..5).unwrap_or(time)).trim().to_string()
}

/// `1/250 s · f/2.8 · ISO 400 · +0.3 EV`, whatever of it the camera recorded.
pub fn exposure_line(e: &Exif) -> String {
    let mut parts = Vec::new();
    if let Some(t) = e.exposure_time.filter(|t| t.n != 0 && t.d != 0) {
        let secs = t.n as f64 / t.d as f64;
        parts.push(if secs < 1.0 { format!("1/{} s", (1.0 / secs).round()) } else { format!("{} s", trim(secs)) });
    }
    if let Some(f) = e.fnumber.filter(|f| f.d != 0) {
        parts.push(format!("f/{}", trim(f.n as f64 / f.d as f64)));
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
    use rawvibes_core::meta::{Rational, SRational};

    #[test]
    fn keywords_split_and_trim() {
        assert_eq!(parse_keywords(" mare, luce ,,alba "), ["mare", "luce", "alba"]);
        assert!(parse_keywords("  ").is_empty());
    }

    #[test]
    fn exposure_reads_like_a_camera() {
        let e = Exif {
            exposure_time: Some(Rational::new(1, 250)),
            fnumber: Some(Rational::new(28, 10)),
            iso_speed_ratings: Some(400),
            exposure_bias: Some(SRational::new(3, 10)),
            ..Default::default()
        };
        assert_eq!(exposure_line(&e), "1/250 s · f/2.8 · ISO 400 · +0.3 EV");
        let long = Exif { exposure_time: Some(Rational::new(2, 1)), fnumber: Some(Rational::new(8, 1)), ..Default::default() };
        assert_eq!(exposure_line(&long), "2 s · f/8");
        assert_eq!(exposure_line(&Exif::default()), "");
    }
}
