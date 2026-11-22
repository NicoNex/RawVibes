//! The Metadata tab: what the camera recorded (read only) and the photo's own
//! title, keywords, rights and location, saved in its sidecar.

use std::cell::RefCell;
use std::rc::Rc;

use relm4::adw::prelude::*;
use relm4::gtk::glib;
use relm4::{ComponentSender, adw, gtk};

use rawvibes_core::meta::{Exif, RawMetadata};
use rawvibes_core::sidecar::Meta;

use crate::{App, Msg, gettext, vbox};

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
    /// The shot card: camera · lens, then exposure, focal length, date, location.
    camera: [gtk::Label; 5],
    fields: Vec<(MetaField, adw::EntryRow, String)>,
    /// Keywords as chips, the entry that adds them, and the list they show.
    chips: gtk::FlowBox,
    keywords: Rc<RefCell<Vec<String>>>,
    sender: ComponentSender<App>,
    /// What the export will carry, with a link to Preferences.
    export_note: gtk::Label,
}

fn list() -> gtk::ListBox {
    gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build()
}

/// A dim caption over a value, as in the shot card.
fn fact(grid: &gtk::Grid, title: &str, col: i32, row: i32, width: i32) -> gtk::Label {
    let b = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).hexpand(true).build();
    b.append(&gtk::Label::builder().label(title).xalign(0.0).css_classes(["caption", "dim-label"]).build());
    let v = gtk::Label::builder().label("—").xalign(0.0).wrap(true).selectable(true).build();
    b.append(&v);
    grid.attach(&b, col, row, width, 1);
    v
}

impl MetaUi {
    pub fn new(sender: &ComponentSender<App>) -> Self {
        let root = vbox(6);
        let grid = gtk::Grid::builder()
            .column_spacing(12)
            .row_spacing(10)
            .column_homogeneous(true)
            .css_classes(["shot-card"])
            .build();
        let headline = fact(&grid, &gettext("Shot"), 0, 0, 2);
        headline.add_css_class("heading");
        let camera = [
            headline,
            fact(&grid, &gettext("Exposure"), 0, 1, 1),
            fact(&grid, &gettext("Focal length"), 1, 1, 1),
            fact(&grid, &gettext("Taken"), 0, 2, 1),
            fact(&grid, &gettext("GPS location"), 1, 2, 1),
        ];
        root.append(&grid);
        let keywords: Rc<RefCell<Vec<String>>> = Rc::default();
        let chips =
            gtk::FlowBox::builder().selection_mode(gtk::SelectionMode::None).column_spacing(6).row_spacing(6).build();
        let mut fields = Vec::new();
        for (heading, group) in [
            (
                gettext("Description"),
                &[(MetaField::Title, gettext("Title")), (MetaField::Caption, gettext("Caption"))][..],
            ),
            (
                gettext("Rights"),
                &[(MetaField::Creator, gettext("Creator")), (MetaField::Copyright, gettext("Copyright"))],
            ),
            (gettext("Location"), &[(MetaField::City, gettext("City")), (MetaField::Country, gettext("Country"))]),
        ] {
            root.append(&crate::phase_label(&heading));
            let l = list();
            for (f, title) in group {
                let row = adw::EntryRow::builder().title(title.as_str()).build();
                let (snd, f) = (sender.clone(), *f);
                row.connect_changed(move |r| snd.input(Msg::MetaEdit(f, r.text().into())));
                l.append(&row);
                fields.push((f, row, title.clone()));
            }
            if fields.len() == 2 {
                // Keywords: chips that a click removes, and an entry that adds (commas split).
                let kw = vbox(6);
                kw.set_margin_top(8);
                kw.set_margin_bottom(8);
                kw.set_margin_start(12);
                kw.set_margin_end(12);
                kw.append(
                    &gtk::Label::builder()
                        .label(gettext("Keywords"))
                        .xalign(0.0)
                        .css_classes(["caption", "dim-label"])
                        .build(),
                );
                kw.append(&chips);
                let add =
                    gtk::Entry::builder().placeholder_text(gettext("Add keywords…")).css_classes(["flat"]).build();
                let (snd, k) = (sender.clone(), keywords.clone());
                add.connect_activate(move |e| {
                    let mut all = k.borrow().clone();
                    all.extend(parse_keywords(&e.text()).into_iter().filter(|w| !k.borrow().contains(w)));
                    e.set_text("");
                    snd.input(Msg::MetaEdit(MetaField::Keywords, all.join(", ")));
                });
                kw.append(&add);
                l.append(&gtk::ListBoxRow::builder().child(&kw).activatable(false).build());
            }
            root.append(&l);
        }
        let export_note = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .use_markup(true)
            .css_classes(["caption", "dim-label"])
            .margin_top(8)
            .build();
        let snd = sender.clone();
        export_note.connect_activate_link(move |_, _| {
            snd.input(Msg::Preferences);
            glib::Propagation::Stop
        });
        root.append(&export_note);
        Self { root, camera, fields, chips, keywords, sender: sender.clone(), export_note }
    }

    /// What the export carries: the policy's `name` and what that means.
    pub fn show_policy(&self, name: &str, hint: &str) {
        let (name, hint) = (glib::markup_escape_text(name), glib::markup_escape_text(hint));
        let prefs = glib::markup_escape_text(&gettext("Preferences"));
        self.export_note.set_markup(&gettext!(
            "On export: <b>{}</b> — {}. <a href=\"prefs\">{}</a>",
            name,
            hint,
            prefs
        ));
    }

    /// The camera's facts of the open photo; dashes when it recorded none.
    pub fn show_camera(&self, src: Option<&RawMetadata>) {
        let e = src.map(|s| &s.exif);
        let camera = src.map(|s| {
            if s.model.starts_with(&s.make) {
                s.model.clone()
            } else {
                format!("{} {}", s.make, s.model).trim().to_string()
            }
        });
        let lens = e
            .and_then(|e| e.lens_model.clone())
            .or_else(|| src.and_then(|s| s.lens.as_ref()).map(|l| format!("{} {}", l.lens_make, l.lens_model)));
        let headline = [camera, lens].into_iter().flatten().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
        let focal = e.and_then(|e| e.focal_length).filter(|r| r.d != 0).map(|r| format!("{} mm", trim(r.as_f32())));
        let taken = e.and_then(|e| e.date_time_original.as_deref()).map(taken_line);
        let location = e.map(|e| {
            if e.gps.as_ref().is_some_and(|g| g.gps_latitude.is_some()) { gettext("Recorded") } else { gettext("None") }
        });
        for (label, v) in self.camera.iter().zip([Some(headline), e.map(exposure_line), focal, taken, location]) {
            label.set_label(v.filter(|v| !v.is_empty()).as_deref().unwrap_or("—"));
        }
    }

    /// Shows `m`; Creator and Copyright name the Preferences' default they fall back to.
    pub fn set(&self, m: &Meta, defaults: &Meta) {
        if *self.keywords.borrow() != m.keywords {
            *self.keywords.borrow_mut() = m.keywords.clone();
            self.chips.remove_all();
            for k in &m.keywords {
                let chip = gtk::Button::builder()
                    .label(format!("{k}  ✕"))
                    .tooltip_text(gettext!("Remove “{}”", k))
                    .css_classes(["pill", "keyword"])
                    .build();
                let (snd, all, k) = (self.sender.clone(), m.keywords.clone(), k.clone());
                chip.connect_clicked(move |_| {
                    let left: Vec<String> = all.iter().filter(|w| **w != k).cloned().collect();
                    snd.input(Msg::MetaEdit(MetaField::Keywords, left.join(", ")));
                });
                self.chips.append(&chip);
            }
        }
        for (f, row, title) in &self.fields {
            let text = slot(&mut m.clone(), *f).map_or_else(|| m.keywords.join(", "), |s| s.clone());
            if row.text() != text {
                row.set_text(&text);
            }
            let fallback = match f {
                MetaField::Creator => &defaults.creator,
                MetaField::Copyright => &defaults.copyright,
                _ => "",
            };
            row.set_title(&if fallback.is_empty() {
                title.clone()
            } else {
                gettext!("{} (empty: {})", title, fallback)
            });
        }
    }
}

/// The text field `f` edits; `None` for the keyword list.
fn slot(m: &mut Meta, f: MetaField) -> Option<&mut String> {
    Some(match f {
        MetaField::Title => &mut m.title,
        MetaField::Caption => &mut m.caption,
        MetaField::Creator => &mut m.creator,
        MetaField::Copyright => &mut m.copyright,
        MetaField::City => &mut m.city,
        MetaField::Country => &mut m.country,
        MetaField::Keywords => return None,
    })
}

/// Puts `text` in `f` of `m`; false when it already held it (an echo of `set`).
pub fn apply(m: &mut Meta, f: MetaField, text: &str) -> bool {
    match slot(m, f) {
        Some(s) if s != text => *s = text.to_string(),
        Some(_) => return false,
        None => {
            let k = parse_keywords(text);
            if k == m.keywords {
                return false;
            }
            m.keywords = k;
        }
    }
    true
}

/// Keywords typed comma-separated, trimmed, empty ones dropped.
pub fn parse_keywords(text: &str) -> Vec<String> {
    text.split(',').map(str::trim).filter(|k| !k.is_empty()).map(String::from).collect()
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
pub fn exposure_line(e: &Exif) -> String {
    let mut parts = Vec::new();
    if let Some(t) = e.exposure_time.filter(|t| t.n != 0 && t.d != 0) {
        let secs = t.as_f32();
        parts.push(if secs < 1.0 { format!("1/{} s", (1.0 / secs).round()) } else { format!("{} s", trim(secs)) });
    }
    if let Some(f) = e.fnumber.filter(|f| f.d != 0) {
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
        let long =
            Exif { exposure_time: Some(Rational::new(2, 1)), fnumber: Some(Rational::new(8, 1)), ..Default::default() };
        assert_eq!(exposure_line(&long), "2 s · f/8");
        assert_eq!(exposure_line(&Exif::default()), "");
    }
}
