//! The editor's flows, ported from the GTK UI's `App` methods: what the widgets showed is now state.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use rawvibes_core::crop::{self, Crop, Ratio};
use rawvibes_core::develop::{Camera, Detail, Develop, WB_PRESETS};
use rawvibes_core::export::PhotoFormat;
use rawvibes_core::gpu::{Done, Job, PhotoExport, ThumbBase};
use rawvibes_core::grade::Grade;
use rawvibes_core::lab::{Blend, Layer, Source, Stack};
use rawvibes_core::lens;
use rawvibes_core::lut::{self, Lut3d, LutEntry, MAX_LIBRARY};
use rawvibes_core::matching::{self, Match};
use rawvibes_core::meta::{self, RawMetadata};
use rawvibes_core::photo::{Decoded, PREVIEW_MAX, Photo, is_photo, is_raw};
use rawvibes_core::preset::{self, Preset};
use rawvibes_core::profiles::Profile;
use rawvibes_core::raw::read_raw;
use rawvibes_core::scopes::{HIST_H, HIST_W, Scopes, VEC_SIZE};
use rawvibes_core::sidecar::{Label, LookRef, Off, Sidecar};
use rawvibes_core::view::{Region, Zoom};

use super::*;

impl App {
    pub fn split_value(&self) -> f32 {
        if self.compare { self.split } else { 0.0 }
    }

    fn active_source(&self) -> Option<&(Arc<Photo>, Profile)> {
        if self.show_chart { self.chart.as_ref() } else { self.photo.as_ref() }
    }

    pub fn has_source(&self) -> bool {
        self.active_source().is_some()
    }

    /// Profile of the pixels currently shown (photo or chart).
    pub fn source_profile(&self) -> Profile {
        self.active_source().map_or(Profile::Standard, |s| s.1)
    }

    /// Sends the active source (photo or test chart) to the GPU and redraws.
    pub(super) fn show_source(&mut self) {
        let Some((img, profile)) = self.active_source().cloned() else {
            self.ui.canvas = "empty";
            return;
        };
        // The shader decodes the profile the pixels were actually encoded in.
        self.grade.params.set_input(profile);
        if self.view_zoom().is_some() {
            self.request_view();
        } else {
            self.send(Job::SetPhoto(img));
            self.sent = None;
        }
        self.ui.canvas = "image";
        self.gallery_stale = true;
        self.refresh();
    }

    /// The grade as previewed and exported: switched-off sections are neutral.
    pub fn graded(&self) -> Grade {
        let mut g = self.grade.clone();
        bypass_off(&mut g, &self.off);
        g
    }

    pub(super) fn render(&mut self) {
        let mut grade = self.graded();
        grade.params.overlay = self.overlay as u32;
        // No HDR rendition for an SDR surface: it would be read back only to be dropped.
        if self.hdr_preview && crate::hdr::surface() {
            grade.params.hdr_stops = self.headroom;
        }
        // Where the shown image sits in the crop, for the creative vignette.
        if let (Some(shown), Some(crop)) = (self.shown_region(), self.crop_region()) {
            let visible = self.view_zoom().map_or(shown, |z| z.view(&shown, self.canvas).0);
            grade.params.frame(&visible, &crop);
        }
        let req = Some((grade, self.split_value()));
        if self.has_source() && self.sent != req {
            self.sent = req;
            let (grade, split) = self.sent.clone().expect("set above");
            self.send(Job::Render(grade, split));
        }
        self.render_roll();
        self.schedule_save();
    }

    /// The open photo's edits, as its sidecar holds them.
    pub(super) fn sidecar_state(&self) -> Sidecar {
        let looks = self
            .looks
            .iter()
            .filter_map(|l| {
                let path = l.path.clone()?;
                Some(LookRef { path, intensity: l.intensity, input: l.input, on: l.on })
            })
            .collect();
        let (mut grade, look_blend) = (self.grade.clone(), self.look_blend);
        // The input decoding belongs to the photo, not its edits (`apply_grade` keeps it).
        let neutral = Grade::default().params;
        (grade.params.input, grade.params.gamut) = (neutral.input, neutral.gamut);
        // The lens profile is the photo's, found again on load: not an edit, nor saved.
        let develop = Develop { detail: Detail { lens: None, ..self.develop.detail }, ..self.develop };
        Sidecar {
            develop,
            grade,
            crop: self.crop,
            ratio: self.ratio,
            looks,
            look_blend,
            rating: self.rating,
            label: self.label,
            profile: Some(self.profile),
            off: self.off.clone(),
            meta: self.meta.clone(),
            lens_profile: self.lens_profile.clone(),
        }
    }

    /// Adds the open photo's edits to its undo history when they changed since the step shown.
    /// Stars, labels and metadata are not edits: they never make a step.
    pub(super) fn record_edits(&mut self) {
        if self.path.is_none() || self.saved.is_none() || self.looks_loading {
            return;
        }
        let now = self.sidecar_state();
        let same =
            |s: &Sidecar| Sidecar { rating: now.rating, label: now.label, meta: now.meta.clone(), ..s.clone() } == now;
        if self.history.get(self.undo_at).is_some_and(|(s, _)| same(s)) {
            return;
        }
        self.history.truncate(self.undo_at + 1);
        self.history.push((now, self.looks.clone()));
        if self.history.len() > HISTORY_MAX {
            self.history.remove(0);
        }
        self.undo_at = self.history.len() - 1;
    }

    pub(super) fn reset_history(&mut self) {
        (self.history, self.undo_at) = (Vec::new(), 0);
        self.record_edits();
    }

    pub fn can_undo(&self) -> bool {
        self.undo_at > 0
    }

    pub fn can_redo(&self) -> bool {
        self.undo_at + 1 < self.history.len()
    }

    /// Shows undo step `back` (true) or forward, keeping the photo's stars, label and metadata.
    pub(super) fn step_history(&mut self, back: bool) {
        self.record_edits(); // an edit made under a second ago is a step too
        let to = if back { self.undo_at.checked_sub(1) } else { Some(self.undo_at + 1) };
        let Some((i, (mut sc, looks))) = to.and_then(|i| Some((i, self.history.get(i)?.clone()))) else { return };
        self.undo_at = i;
        (sc.rating, sc.label) = (self.rating, self.label);
        self.restore_edits((sc, looks));
    }

    /// Puts `edits` on the open photo: develop, grade, crop and looks.
    pub(super) fn restore_edits(&mut self, (mut sc, looks): Edits) {
        let develop = self.develop;
        // Metadata is not an edit: undo, paste and undoing a reset leave what was typed.
        sc.meta = self.meta.clone();
        self.apply_grade(Grade::default());
        sc.looks.clear();
        self.restore_sidecar(sc);
        self.replace_looks(looks);
        if self.develop != develop {
            self.develop_changed();
        }
        self.crop_changed();
    }

    /// The open photo's stars and label onto its filmstrip card.
    pub(super) fn show_marks(&mut self) {
        let (rating, label) = (self.rating, self.label);
        // By path: while another photo loads, `active` already points at it.
        if let Some(it) = self.roll.iter_mut().find(|it| Some(&it.path) == self.path.as_ref()) {
            (it.rating, it.label) = (rating, label);
        }
    }

    /// The filmstrip filter shows a photo with these marks.
    pub fn shows(&self, rating: u8, label: Label) -> bool {
        shown(self.ui.roll_filter, rating, label)
    }

    /// Pastes the copied edits onto every photo the filmstrip shows but the open one,
    /// straight into their sidecars. Returns how many took them, and the failures.
    pub(super) fn paste_all(&mut self, (edits, looks): &Edits) -> (usize, Vec<String>) {
        let (mut n, mut failed) = (0, Vec::new());
        let filter = self.ui.roll_filter;
        for (i, it) in self.roll.iter_mut().enumerate() {
            if Some(i) == self.active || !shown(filter, it.rating, it.label) {
                continue;
            }
            let r = Sidecar::load(&it.path).and_then(|sc| {
                let sc = pasted(edits, &sc.unwrap_or_default());
                sc.save(&it.path).map(|_| sc)
            });
            match r {
                Ok(sc) => {
                    (it.grade, it.looks) = (card_grade(&sc), looks.clone());
                    n += 1;
                }
                Err(e) => failed.push(format!("{}: {e}", file_label(&it.path))),
            }
        }
        (n, failed)
    }

    pub(super) fn batch_stop(&mut self) {
        self.batch_end(&gettext("Export stopped"));
    }

    /// Ends the batch with `toast`, and reopens the photo open before it.
    fn batch_end(&mut self, toast: &str) {
        let Some(b) = self.batch.take() else { return };
        self.ui.export.busy = false;
        self.toast(toast);
        if let Some(i) = b.back.and_then(|p| self.roll.iter().position(|it| it.path == p))
            && Some(i) != self.active
        {
            self.load_active(i);
        }
    }

    /// Exports the next photo of the batch, or ends it.
    pub(super) fn batch_next(&mut self) {
        if self.batch.as_ref().is_some_and(|b| b.stop) {
            return self.batch_stop();
        }
        let Some(b) = self.batch.as_mut() else { return };
        let Some(path) = b.left.pop() else {
            let done = b.total - b.failed.len();
            let toast = if b.failed.is_empty() {
                gettext!("{} photos exported to {}", done, b.dir.display())
            } else {
                gettext!("{} photos exported, {} failed: {}", done, b.failed.len(), b.failed.join("; "))
            };
            self.batch_end(&toast);
            self.ui.export.open = false;
            return;
        };
        let i = b.total - b.left.len();
        self.ui.export.step = gettext!("Photo {} of {}", i, b.total);
        b.waiting = Some(path.clone());
        match self.roll.iter().position(|it| it.path == path) {
            Some(i) if Some(i) != self.active || self.decoded.is_none() => self.load_active(i),
            Some(_) => self.batch_ready(),
            None => {
                if let Some(b) = self.batch.as_mut() {
                    b.failed.push(gettext!("{}: no longer in the filmstrip", file_label(&path)));
                }
                self.batch_next();
            }
        }
    }

    /// Exports the batch's photo once it is open, its looks loaded.
    pub(super) fn batch_ready(&mut self) {
        let Some(b) = self.batch.as_mut() else { return };
        if b.waiting.is_none() || b.waiting != self.path || self.looks_loading || self.decoded.is_none() {
            return;
        }
        let path = b.waiting.take().expect("checked above");
        let ext = self.ui.export.format.unwrap_or(PhotoFormat::Jpeg).ext();
        let stem = stem_label(&path).unwrap_or_default();
        // Never over an existing file: a RAW+JPEG pair, or a photo of the strip itself.
        let out = free_path(&b.dir, &stem, ext, &export_suffix());
        if !self.export_to(out) {
            if let Some(b) = self.batch.as_mut() {
                b.failed.push(gettext!("{}: cannot export", file_label(&path)));
            }
            self.batch_next();
        }
    }

    /// The open RAW's lens profile from the database: the one picked, or the one its EXIF names.
    pub(super) fn find_lens(&self) -> LensView {
        let src = self.camera_meta();
        let exif = src.map(|m| &m.exif);
        let value = |r: Option<meta::Rational>| r.filter(|r| r.d != 0).map_or(0.0, |r| r.as_f32());
        let (exif_focal, aperture) = (value(exif.and_then(|e| e.focal_length)), value(exif.and_then(|e| e.fnumber)));
        let distance = value(exif.and_then(|e| e.subject_distance));
        let mut view = LensView {
            profile: None,
            lens: None,
            status: LensStatus::NoDatabase,
            focal: exif_focal,
            aperture,
            asked: false,
        };
        let Some(db) = self.lens_db.as_deref() else { return view };
        let camera = src.and_then(|m| db.find_camera(&m.make, &m.model));
        let (lens, chosen) = match &self.lens_profile {
            Some(c) if c.model.is_empty() => return LensView { status: LensStatus::Off, ..view },
            Some(c) => {
                let pick = |typed: f32, exif: f32| if typed > 0.0 { typed } else { exif };
                (view.focal, view.aperture) = (pick(c.focal, exif_focal), pick(c.aperture, aperture));
                (db.lens(&c.maker, &c.model), true)
            }
            None => (db.find_lens(camera, "", &src.and_then(meta::lens_name).unwrap_or_default()), false),
        };
        let Some(found) = lens else { return LensView { status: LensStatus::NotFound, asked: chosen, ..view } };
        let name = found.model.clone();
        view.lens = Some((found.maker.clone(), name.clone()));
        match lens::calib_at(found, view.focal, view.aperture, distance) {
            Some(c) => LensView {
                profile: Some(lens::Profile::new(found, c, camera.map(|c| c.crop))),
                status: if chosen { LensStatus::Chosen(name) } else { LensStatus::Auto(name) },
                // A manual lens records no focal length: its row stays open to change it.
                asked: chosen || exif_focal <= 0.0,
                ..view
            },
            None => LensView { status: LensStatus::NeedsFocal(name), asked: true, ..view },
        }
    }

    /// Puts the open RAW's lens profile into the development and the Lens section;
    /// re-develops when it changed and `redevelop` says so.
    pub(super) fn update_lens(&mut self, redevelop: bool) {
        let LensView { profile, status, focal, aperture, asked, .. } = self.find_lens();
        let changed = self.develop.detail.lens != profile;
        self.develop.detail.lens = profile;
        (self.ui.lens_status, self.ui.lens_focal, self.ui.lens_aperture, self.ui.lens_asked) =
            (Some(status), focal, aperture, asked);
        if changed && redevelop && self.camera().is_some() {
            self.develop_changed();
        }
    }

    pub fn has_lens_profile(&self) -> bool {
        self.develop.detail.lens.is_some()
    }

    /// What the open photo's camera recorded; never another photo's, whose load is still on its way.
    pub fn camera_meta(&self) -> Option<&RawMetadata> {
        self.source_meta.as_ref().filter(|(p, _)| Some(p) == self.path.as_ref()).map(|(_, m)| &**m)
    }

    /// The metadata an export of `crop` embeds: the photo's, with the author defaults, as Preferences allow.
    fn export_tags(&self, crop: &Region) -> meta::Tags {
        let src = self.camera_meta();
        let m = meta::with_defaults(&self.meta, &self.prefs.author, src.and_then(meta::capture_year));
        meta::tags(src, &m, self.prefs.policy, Decoded::fit_size(crop, u32::MAX))
    }

    /// Starts exporting the open photo to `path`; false if there is none.
    pub(super) fn export_to(&mut self, path: PathBuf) -> bool {
        let (Some(decoded), Some(crop)) = (self.decoded.clone(), self.crop_region()) else { return false };
        let cancel = Arc::new(AtomicBool::new(false));
        self.exporting = Some(cancel.clone());
        if self.batch.is_none() {
            self.ui.export.busy = true;
        }
        let format = self.ui.export.format.unwrap_or(PhotoFormat::Jpeg);
        let (quality, headroom) = (self.ui.export.quality, self.headroom);
        // Remembered as used, not per spin tick.
        let _ = settings::set_value(settings::EXPORT_FORMAT, format.tag());
        let _ = settings::set_value(settings::JPEG_QUALITY, &quality.to_string());
        let tags = self.export_tags(&crop);
        if matches!(format, PhotoFormat::Jpeg | PhotoFormat::UltraHdr)
            && tags.xmp.as_deref().is_some_and(|x| !export::xmp_fits_jpeg(x))
        {
            self.toast(&gettext("Title, keywords and rights are too long for a JPEG: exported without them"));
        }
        let job = Job::ExportPhoto(Box::new(PhotoExport {
            decoded,
            crop,
            develop: self.applied_develop(),
            profile: self.profile,
            grade: self.graded(),
            look: self.look_stack(self.look_layers()),
            format,
            quality,
            headroom,
            path,
            cancel,
            tags,
        }));
        self.send(job);
        true
    }

    /// The dialog's button: choose the file, or stop the running export.
    pub(super) fn export_go(&mut self) {
        if let Some(b) = self.batch.as_mut() {
            b.stop = true;
        }
        if let Some(cancel) = &self.exporting {
            return cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        if self.batch.is_some() {
            return self.batch_stop();
        }
        if self.ui.export.all {
            let folder = self.path.as_ref().and_then(|p| p.parent()).map(Path::to_path_buf);
            let options = serde_json::json!({ "title": gettext("Export Photos To"), "folder": folder });
            return self.ask("folder", options, |a| a.path().map(Msg::BatchTo));
        }
        let Some(path) = self.path.clone() else { return };
        let format = self.ui.export.format.unwrap_or(PhotoFormat::Jpeg);
        let stem = stem_label(&path).unwrap_or_default();
        self.ask(
            "save",
            serde_json::json!({
                "title": gettext("Export Photo"),
                // Never the original's own name: a JPEG source sits in the same folder.
                "name": edit_name(&stem, format.ext(), &export_suffix()),
                "filters": [format!("{} (*.{})", format.ext().to_uppercase(), format.ext())],
                "folder": path.parent(),
            }),
            |a| a.path().map(Msg::ExportPhotoTo),
        );
    }

    /// Replaces the look stack: a sidecar load still in flight becomes stale. Returns the stack it replaced.
    pub(super) fn replace_looks(&mut self, looks: Vec<LabLayer>) -> Vec<LabLayer> {
        self.looks_gen += 1;
        self.looks_loading = false;
        let old = std::mem::replace(&mut self.looks, looks);
        self.layers_changed(Layers::Look, true);
        old
    }

    /// Writes the sidecar shortly after the last edit, and makes it an undo step a second after.
    pub(super) fn schedule_save(&mut self) {
        if self.saved.is_none() {
            return;
        }
        self.timers.debounce(WRITE_SETTLE, Msg::WriteSidecar);
        self.timers.debounce(SAVE_SETTLE, Msg::SaveSidecar);
    }

    /// Writes the open photo's sidecar if its edits changed since it was read or written.
    pub(super) fn save_now(&mut self) {
        let Some(path) = self.path.clone().filter(|_| !self.looks_loading) else { return };
        let now = self.sidecar_state();
        if self.saved.as_ref().is_none_or(|s| *s == now) {
            return;
        }
        match now.save(&path) {
            Ok(()) => self.saved = Some(now),
            Err(e) if !std::mem::replace(&mut self.save_failed, true) => {
                self.toast(&gettext!("Cannot save the edits next to the photo: {}", e))
            }
            Err(_) => {}
        }
    }

    /// The photo's sidecar into the editor, onto a neutral grade without looks.
    pub(super) fn restore_sidecar(&mut self, sc: Sidecar) {
        (self.rating, self.label, self.meta) = (sc.rating, sc.label, sc.meta);
        self.lens_profile = sc.lens_profile;
        self.set_offs(&sc.off);
        // Another profile re-encodes what is shown in it (GTK's dropdown echo did this).
        if let Some(p) = sc.profile.filter(|&p| p != self.profile) {
            self.profile = p;
            self.reencode_roll();
            if self.show_chart {
                self.input(Msg::Chart(true));
            }
            self.encode_photo();
        }
        self.develop = sc.develop;
        // The profile is the photo's, never the snapshot's (undo, paste): found again.
        self.update_lens(false);
        let row = crop::PRESETS
            .iter()
            .position(|(_, r)| {
                *r == sc.ratio
                    || matches!((r, sc.ratio), (Ratio::Fixed(a, b), Ratio::Fixed(c, d)) if (*a, *b) == (d, c))
            })
            .unwrap_or(0);
        self.restore_crop(sc.crop, sc.ratio, row as u32);
        self.apply_grade(sc.grade);
        self.look_blend = sc.look_blend;
        if !sc.looks.is_empty() {
            self.looks_loading = true;
            let (s, refs, generation) = (self.tx.clone(), sc.looks, self.looks_gen);
            std::thread::spawn(move || {
                let loaded = refs.into_iter().map(|r| {
                    let lut = Lut3d::load(&r.path).map(Arc::new);
                    (r, lut)
                });
                s.send(Msg::SidecarLooks(generation, loaded.collect()));
            });
        }
    }

    /// Each filmstrip frame's grade: the open photo's as edited, the others' from their sidecars.
    pub(super) fn roll_grades(&self) -> Vec<Grade> {
        let current = self.graded();
        let mut grades: Vec<Grade> = self
            .roll
            .iter()
            .map(|it| {
                let mut g = it.grade.clone();
                (g.params.input, g.params.gamut) = (current.params.input, current.params.gamut);
                g
            })
            .collect();
        if let Some(g) = self.active.and_then(|i| grades.get_mut(i)) {
            *g = current;
        }
        grades
    }

    /// The filmstrip through each frame's grade, once a drag settles: the main preview never waits.
    pub(super) fn render_roll(&mut self) {
        let roll_current = self.roll.is_empty() || self.roll_sent.as_ref() == Some(&self.roll_grades());
        if roll_current && !self.browsing {
            return;
        }
        self.timers.debounce(ROLL_SETTLE, Msg::RollTick);
    }

    /// Hands the filmstrip to the GPU; `layout` when photos were added or removed.
    pub(super) fn upload_roll(&mut self, layout: bool) {
        if layout {
            self.roll_gen += 1;
        }
        let blank = Arc::new(Photo { width: 1, height: 1, pixels: vec![[0.0, 0.0, 0.0, 1.0]] });
        let photos =
            self.roll.iter().map(|it| it.encoded.as_ref().map_or_else(|| blank.clone(), |e| e.1.clone())).collect();
        self.send(Job::SetRoll(self.roll_gen, photos));
        self.roll_sent = None;
        self.render_roll();
    }

    /// Re-encodes, off the main thread, the strip photos shown in another profile.
    pub(super) fn reencode_roll(&self) {
        let profile = self.profile;
        let stale: Vec<(PathBuf, Decoded)> = self
            .roll
            .iter()
            .filter(|it| it.encoded.as_ref().is_none_or(|e| e.0 != profile))
            .filter_map(|it| Some((it.path.clone(), it.small.clone()?)))
            .collect();
        if stale.is_empty() {
            return;
        }
        let s = self.tx.clone();
        std::thread::spawn(move || {
            let encoded = stale.into_iter().map(|(p, small)| (p, small.encode(profile, &Develop::default()))).collect();
            s.send(Msg::RollEncoded(profile, encoded));
        });
    }

    /// Takes photo `i` out of the strip; true if it was the one being edited.
    pub(super) fn drop_from_roll(&mut self, i: usize) -> bool {
        let gone = self.roll.remove(i);
        self.drop_image(&format!("roll/{}", gone.id));
        let was_active = self.active == Some(i);
        self.active = self.active.and_then(|a| if a > i { Some(a - 1) } else { (a < i).then_some(a) });
        self.roll_changed();
        was_active
    }

    /// Shows the match panel as the grade has it.
    pub(super) fn show_match_panel(&mut self) {
        self.ui.matching = if self.grade.params.match_amount > 0.0 { "set" } else { "empty" };
    }

    pub(super) fn cancel_match(&mut self) {
        self.match_gen += 1;
        self.show_match_panel();
    }

    pub(super) fn show_match_thumb(&mut self, reference: &Photo) {
        self.show_image("match".into(), rgba8(&reference.pixels), reference.width, reference.height, false);
    }

    /// Matches the photo on screen (as decoded, before any grading) to `reference`.
    pub(super) fn request_match(&mut self, reference: Arc<Photo>) {
        let small = self.active.and_then(|i| self.roll.get(i)).and_then(|it| it.encoded.clone());
        let source = match (small, self.active_source()) {
            (Some((profile, photo)), Some((_, shown))) if profile == *shown && !self.show_chart => photo,
            (_, Some((photo, _))) => photo.clone(),
            (_, None) => return self.toast(&no_photo_to_match()),
        };
        self.camera_job = None;
        self.match_gen += 1;
        self.ui.matching = "busy";
        let job = Job::Match { generation: self.match_gen, source, reference, input: self.grade.neutralized() };
        self.send(job);
    }

    /// After photos were added or removed.
    pub(super) fn roll_changed(&mut self) {
        // Remembered for the next start; a failed write only loses the resume.
        let roll: Vec<String> = self.roll.iter().map(|it| it.path.to_string_lossy().into_owned()).collect();
        let _ = settings::set_value(settings::ROLL, &roll.join("\n"));
        self.show_marks();
        if self.roll.len() < 2 {
            self.ui.grid = false;
        }
        self.upload_roll(true);
    }

    /// Adds `paths` to the filmstrip and opens `open_at`, or else the first of them.
    pub(super) fn add_to_roll(&mut self, paths: Vec<PathBuf>, open_at: Option<PathBuf>) {
        let (photos, others): (Vec<PathBuf>, Vec<PathBuf>) = paths.into_iter().partition(|p| is_photo(p));
        if !others.is_empty() {
            self.toast(&gettext!("{} files skipped: not JPEG, TIFF, PNG or camera RAW", others.len()));
        }
        let mut fresh = Vec::new();
        let mut show = None;
        for p in photos {
            match self.roll.iter().position(|it| it.path == p) {
                Some(i) => show = show.or(Some(i)),
                None if self.roll.len() < MAX_ROLL => {
                    show = show.or(Some(self.roll.len()));
                    // shortcut: sidecars read on the main thread, small JSON; move to the loader if it shows.
                    let sc = Sidecar::load(&p).ok().flatten().unwrap_or_default();
                    self.roll.push(RollItem {
                        path: p.clone(),
                        grade: card_grade(&sc),
                        rating: sc.rating,
                        label: sc.label,
                        looks: Vec::new(),
                        small: None,
                        encoded: None,
                        id: self.next_roll_id,
                    });
                    self.next_roll_id += 1;
                    fresh.push(p);
                }
                None => {
                    self.toast(&gettext!("The filmstrip holds up to {} photos", MAX_ROLL));
                    break;
                }
            }
        }
        if let Some(p) = open_at {
            show = self.roll.iter().position(|it| it.path == p).or(show);
        }
        let open = show.filter(|&i| Some(i) != self.active || !self.has_source());
        if !fresh.is_empty() {
            self.roll_changed();
            // The photo about to be opened gets its small copy from that decode.
            let opening = open.map(|i| self.roll[i].path.clone());
            fresh.retain(|p| Some(p) != opening.as_ref());
            let (s, profile) = (self.tx.clone(), self.profile);
            std::thread::spawn(move || {
                for p in fresh {
                    let small = Decoded::load_max(&p, Some(ROLL_PX)).map(|d| roll_copy(&d, profile));
                    s.send(Msg::RollLoaded(p, small));
                }
            });
        }
        if let Some(i) = open {
            self.load_active(i);
        }
    }

    /// Opens filmstrip photo `i` for editing (full-size decode off the main thread).
    pub(super) fn load_active(&mut self, i: usize) {
        let Some(path) = self.roll.get(i).map(|it| it.path.clone()) else { return };
        self.active = Some(i);
        let _ = settings::set_value(settings::LAST_PHOTO, &path.to_string_lossy());
        if let Some(dir) = path.parent() {
            let _ = settings::set_folder(settings::PHOTO_FOLDER, dir);
        }
        self.ui.canvas = if self.has_source() { "image" } else { "loading" };
        self.load_gen += 1;
        let (s, generation, profile) = (self.tx.clone(), self.load_gen, self.profile);
        std::thread::spawn(move || {
            // Headers only, before queuing behind another decode.
            s.send(Msg::SourceMeta(generation, path.clone(), meta::read(&path).map(Arc::new)));
            // One full-resolution decode at a time: each peaks above 1 GB on a 24 MP RAW.
            static FULL_DECODE: std::sync::Mutex<()> = std::sync::Mutex::new(());
            let _one = FULL_DECODE.lock().unwrap_or_else(|e| e.into_inner());
            if is_raw(&path) {
                // Developed on the GPU: the strip copy comes back with it (Done::RawLoaded).
                return s.send(Msg::RawRead(generation, path.clone(), read_raw(&path).map(Arc::new)));
            }
            let r = Decoded::load(&path);
            let small = r.as_ref().map(|d| roll_copy(d, profile)).map_err(String::clone);
            s.send(Msg::RollLoaded(path.clone(), small));
            s.send(Msg::Loaded(generation, path, r));
        });
    }

    /// A photo decoded: it becomes the one edited, with its sidecar's edits.
    pub(super) fn loaded(&mut self, path: PathBuf, decoded: Decoded) {
        self.save_now(); // the previous photo's last edits
        // Each photo has its own grade and looks: the previous one keeps its frame's.
        let left = self.replace_looks(Vec::new());
        let graded = self.graded();
        let previous = self.path.as_ref().and_then(|p| self.roll.iter_mut().find(|it| it.path == *p));
        if let Some(it) = previous {
            it.grade = graded;
            it.looks = left;
        }
        self.apply_grade(Grade::default());
        self.look_blend = Blend::Chain;
        self.ui.picker = false;
        self.zoom = None;
        self.reset_crop();
        self.decoded = Some(decoded);
        self.path = Some(path.clone());
        self.develop = Develop::default();
        (self.rating, self.label, self.meta) = (0, Label::None, Default::default());
        self.lens_profile = None;
        self.drafting = false;
        self.ui.ai_msg = None;
        // The sidecar sets the eyes itself.
        match Sidecar::load(&path) {
            Ok(Some(sc)) => self.restore_sidecar(sc),
            Ok(None) => {
                self.set_offs(&[]);
                if self.camera().is_some() && self.prefs.as_in_camera {
                    self.input(Msg::AsInCamera(CameraPart::All));
                }
            }
            Err(e) => {
                self.set_offs(&[]);
                self.toast(&gettext!("Cannot read the edits saved for {}: {}", file_label(&path), e))
            }
        }
        self.update_lens(false);
        // Left earlier this session: that stack is newer than the sidecar's and keeps path-less layers.
        if let Some(looks) =
            self.roll.iter().find(|it| it.path == path).map(|it| it.looks.clone()).filter(|l| !l.is_empty())
        {
            self.replace_looks(looks);
        }
        // Nothing is written until something changes from what was just read.
        self.saved = Some(self.sidecar_state());
        self.show_marks();
        self.reset_history();
        self.batch_ready();
        // Opening a photo means you want to see it.
        self.show_chart = false;
        self.encode_photo();
    }

    /// Replaces the grade (preset, undo, reset). The input decoding stays: it belongs to the photo.
    pub(super) fn apply_grade(&mut self, mut grade: Grade) {
        (grade.params.input, grade.params.gamut) = (self.grade.params.input, self.grade.params.gamut);
        self.grade = grade;
        self.cancel_match();
        if self.grade.params.match_amount <= 0.0 {
            self.match_file = false;
        }
        if !self.match_file {
            self.ui.match_name = gettext("Matched by a preset");
        }
        self.sent = None;
        self.render();
    }

    pub fn match_file(&self) -> bool {
        self.match_file
    }

    pub(super) fn presets(&self) -> Vec<(Preset, PathBuf)> {
        preset::list(&settings::presets_dir())
    }

    /// The grade as seen (switched-off sections stay off), as a preset.
    pub(super) fn current_preset(&self, name: String) -> Preset {
        Preset { name, grade: self.graded().neutral_input() }
    }

    /// The LUT browser shows entry `l` with the current folder and search filters.
    pub fn gallery_shows(&self, l: &LutEntry) -> bool {
        let in_group = self.group.as_ref().is_none_or(|g| *g == l.group);
        let found = self.query.is_empty()
            || l.title.to_lowercase().contains(&self.query)
            || l.group.to_lowercase().contains(&self.query);
        in_group && found
    }

    /// A selected LUT the filters now hide is no longer previewed.
    pub(super) fn hide_filtered_selection(&mut self) {
        if self.selected.and_then(|i| self.luts.get(i)).is_some_and(|l| !self.gallery_shows(l)) {
            self.clear_selection();
        }
    }

    pub(super) fn clear_selection(&mut self) {
        self.selected = None;
        self.drop_candidate();
    }

    /// Hands the Grade page's result to the browser thumbnails; true when it changed.
    fn sync_thumb_base(&mut self) -> bool {
        let input = self.source_profile();
        let (grade, looks) = (self.graded(), self.look_layers());
        let luts = looks
            .iter()
            .filter_map(|l| match &l.source {
                Source::Lut(lut) => Some((lut.clone(), l.intensity, l.input)),
                _ => None,
            })
            .collect();
        let key = Some(BaseKey { grade: grade.clone(), luts, input, blend: self.look_blend });
        if self.base_key == key {
            return false;
        }
        self.base_key = key;
        // An untouched grade keeps the exact direct path (no lattice round trip).
        let base = (!grade.is_neutral() || !looks.is_empty()).then(|| ThumbBase {
            grade,
            looks: self.look_stack(looks),
            input,
        });
        self.send(Job::SetThumbBase(base));
        true
    }

    /// Thumbnails, when the browser is open and what they show changed.
    pub(super) fn refresh_gallery(&mut self) {
        if self.browsing && (self.sync_thumb_base() || self.gallery_stale) {
            self.render_gallery();
        }
    }

    fn render_gallery(&mut self) {
        if self.has_source() && !self.luts.is_empty() {
            self.gallery_stale = false;
            self.gallery_gen += 1;
            self.send(Job::Gallery(self.gallery_gen, self.lut_paths.clone()));
        }
    }

    /// The LUT library scanned: the browser's cards, keeping what the user was looking at.
    pub(super) fn gallery_loaded(&mut self, luts: Vec<LutEntry>, errors: Vec<String>, truncated: bool) {
        if !errors.is_empty() {
            self.toast(&gettext!("{} files skipped: {}", errors.len(), errors.join("; ")));
        }
        let path_of = |i: usize| self.luts.get(i).map(|l| l.path.clone());
        let keep_selected = self.selected.and_then(path_of);
        let keep_candidate = self.candidate.as_ref().and_then(|c| path_of(c.0));
        let keep_group = self.group.take();
        if truncated {
            self.toast(&gettext!("Very large library: showing the first {} LUTs", MAX_LIBRARY));
        }
        let old: Vec<String> = self.frames_with("thumb/");
        for key in old {
            self.drop_image(&key);
        }
        self.group_names = luts.iter().map(|l| l.group.clone()).collect();
        self.group_names.dedup(); // sorted by group in core
        self.group = keep_group.filter(|g| self.group_names.contains(g));
        self.lut_paths = Arc::new(luts.iter().map(|l| l.path.clone()).collect());
        self.luts = luts;
        let find = |p: Option<PathBuf>| p.and_then(|p| self.luts.iter().position(|l| l.path == p));
        self.selected = find(keep_selected);
        match (find(keep_candidate), self.candidate.as_mut()) {
            (Some(i), Some(c)) => c.0 = i,
            _ => self.drop_candidate(),
        }
        self.gallery_stale = true;
        self.refresh_gallery();
    }

    /// Renders only the view on screen; the others refresh when shown.
    pub(super) fn refresh(&mut self) {
        if self.in_lab() { self.render_lab() } else { self.render() }
    }

    fn job_layer(&self, l: &LabLayer) -> Layer {
        let (source, input) = match &l.kind {
            LabKind::Lut(lut) => (Source::Lut(lut.clone()), l.input),
            LabKind::Grade => (Source::Grade(Box::new(self.graded())), self.source_profile()),
            LabKind::Convert(p) => (Source::Convert(*p), *p),
        };
        Layer { source, intensity: l.intensity, input }
    }

    pub(super) fn lab_stack(&self, input: Profile) -> Stack {
        let layers = self.lab_layers.iter().filter(|l| l.on).map(|l| self.job_layer(l)).collect();
        Stack { input, layers, blend: self.lab_blend }
    }

    pub(super) fn render_lab(&mut self) {
        if self.has_source() && !self.lab_layers.is_empty() {
            // The preview shows the stack on the current source, so its input is the source's profile.
            let job = Job::Lab { stack: self.lab_stack(self.source_profile()), split: self.split_value() };
            self.send(job);
        }
    }

    /// The open file's crop fitted to `PREVIEW_MAX` in the current profile, off the main thread.
    pub(super) fn encode_photo(&mut self) {
        let (Some(d), Some(r)) = (self.decoded.clone(), self.shown_region()) else { return };
        self.encoded = Some(r);
        if std::mem::replace(&mut self.encoding, true) {
            self.encode_again = true;
            return;
        }
        let profile = self.profile;
        if d.is_raw() {
            // Best also renders HiDPI canvases at their full device size.
            let max = match self.quality {
                PreviewQuality::Best => PREVIEW_MAX.max(self.canvas[0].max(self.canvas[1]) as u32),
                _ => PREVIEW_MAX,
            };
            let (width, height) = self.draft_size(Decoded::fit_size(&r, max));
            let develop = self.applied_develop();
            return self.send(Job::DevelopRaw {
                generation: 0,
                region: r,
                width,
                height,
                develop,
                profile,
                view: false,
            });
        }
        let s = self.tx.clone();
        std::thread::spawn(move || {
            s.send(Msg::Encoded(profile, d.encode_fit(&r, PREVIEW_MAX, profile, &Develop::default())))
        });
    }

    pub fn camera(&self) -> Option<&Camera> {
        self.decoded.as_ref()?.camera()
    }

    pub(super) fn set_offs(&mut self, offs: &[Off]) {
        self.off = offs.to_vec();
        self.off.sort();
    }

    /// The development the preview and export use: `develop` without the sections switched off.
    pub(super) fn applied_develop(&self) -> Develop {
        let mut d = self.develop;
        self.off.iter().for_each(|o| {
            if let Off::Develop(p) = o {
                p.neutral(&mut d)
            }
        });
        d
    }

    /// Applies a Develop edit; re-develops only when something changed.
    pub(super) fn edit_develop(&mut self, edit: DevelopEdit) {
        let Some(cam) = self.camera().cloned() else { return };
        let mut dev = self.develop;
        let [k, t] = dev.wb.unwrap_or_else(|| cam.as_shot_kelvin_tint());
        match edit {
            DevelopEdit::AsShot | DevelopEdit::Preset(0) => dev.wb = None,
            DevelopEdit::Preset(i) => match WB_PRESETS.get(i as usize - 1) {
                Some(&(_, k, t)) => dev.wb = Some([k, t]),
                None => return, // Custom: whatever the sliders say
            },
            DevelopEdit::Kelvin(v) => dev.wb = Some([v, t]),
            DevelopEdit::Tint(v) => dev.wb = Some([k, v]),
            DevelopEdit::Exposure(v) => dev.exposure = v,
            DevelopEdit::CameraExposure => dev.exposure = cam.baseline_ev,
            DevelopEdit::Detail(field, v) => *field(&mut dev.detail) = v,
            DevelopEdit::CaCorrect(on) => dev.detail.ca_correct = on,
            DevelopEdit::LensTca(on) => dev.detail.lens_tca = on,
            DevelopEdit::NrAi(on) => dev.detail.nr_ai = on,
            DevelopEdit::Picked(kt) => {
                dev.wb = Some(kt);
                self.ui.picker = false;
            }
            DevelopEdit::Pick(on) => {
                self.ui.picker = on;
                return;
            }
        }
        if dev != self.develop {
            self.develop = dev;
            self.develop_changed();
        }
    }

    /// The white balance shown: the set one, or the camera's as shot.
    pub fn kelvin_tint(&self) -> Option<[f32; 2]> {
        let cam = self.camera()?;
        Some(self.develop.wb.unwrap_or_else(|| cam.as_shot_kelvin_tint()))
    }

    /// The white-balance dropdown row: 0 As shot, then `WB_PRESETS`, then Custom.
    pub fn wb_row(&self) -> u32 {
        match self.develop.wb {
            None => 0,
            Some(kt) => WB_PRESETS
                .iter()
                .position(|&(_, k, t)| [k, t] == kt)
                .map_or(WB_PRESETS.len() as u32 + 1, |i| i as u32 + 1),
        }
    }

    /// The point of the oriented full-resolution image under a canvas point.
    pub(super) fn image_point(&self, p: [f32; 2]) -> Option<[f32; 2]> {
        let crop = self.shown_region()?;
        let (r, scale) = match self.view_zoom() {
            Some(z) => (z.view(&crop, self.canvas).0, z.scale),
            None => (crop, Zoom::fit_scale(&crop, self.canvas)),
        };
        Some(r.point([(p[0] - self.canvas[0] / 2.0) * scale, (p[1] - self.canvas[1] / 2.0) * scale]))
    }

    /// Loads parts of `m` into the grade and shows the match panel as set.
    fn apply_match(&mut self, m: &Match) {
        matching::apply(&mut self.grade, m);
        self.ui.matching = "set";
        self.render();
    }

    /// The develop as the camera shot it: as-shot white balance and exposure.
    pub(super) fn as_shot(&self) -> Option<Develop> {
        self.camera().map(|cam| Develop { wb: None, exposure: cam.baseline_ev, ..self.develop })
    }

    /// "As in camera": WB and exposure from the metadata plus the matched tone curve (`All`),
    /// or the full match to the camera JPEG (`Match`, computed once per file).
    pub(super) fn as_in_camera(&mut self, part: CameraPart) {
        let (Some(path), true) = (self.path.clone(), self.camera().is_some()) else { return };
        if matches!(part, CameraPart::All) {
            self.edit_develop(DevelopEdit::AsShot);
            self.edit_develop(DevelopEdit::CameraExposure);
            let (Some(d), Some(develop)) = (self.decoded.clone(), self.as_shot()) else { return };
            let library: Vec<PathBuf> = self.luts.iter().map(|l| l.path.clone()).collect();
            let cached = self.camera_jpeg.clone().filter(|c| c.0 == path).map(|c| c.1);
            let s = self.tx.clone();
            std::thread::spawn(move || {
                let jpeg = cached.or_else(|| rawvibes_core::photo::embedded_preview(&path).ok().map(Arc::new));
                let curve = jpeg.as_ref().and_then(|jpeg| {
                    // The master curve works on display values: Standard whatever the input profile.
                    let neutral = as_shot_render(&d, &develop, Profile::Standard);
                    rawvibes_core::histmatch::auto_tone_curve(&neutral, jpeg, d.size().0)
                });
                s.send(Msg::CameraCurve(path.clone(), jpeg, curve));
                // The Real Time LUTs are offered in place of the curve, not on top of it.
                for l in rawvibes_core::makernote::real_time_luts(&path) {
                    let remembered = settings::value(&camera_lut_key(&l.name)).map(PathBuf::from);
                    let file = remembered
                        .filter(|p| p.is_file())
                        .or_else(|| lut::find_named(library.iter().map(PathBuf::as_path), &l.name));
                    s.send(Msg::CameraLut(path.clone(), l, file));
                }
            });
            return;
        }
        if let Some((_, m)) = self.camera_match.clone().filter(|c| c.0 == path) {
            return self.apply_match(&m);
        }
        if let Some((_, jpeg)) = self.camera_jpeg.clone().filter(|c| c.0 == path) {
            return self.input(Msg::CameraJpeg(path, Ok(jpeg)));
        }
        self.ui.matching = "busy";
        let s = self.tx.clone();
        std::thread::spawn(move || {
            let r = rawvibes_core::photo::embedded_preview(&path).map(Arc::new);
            s.send(Msg::CameraJpeg(path, r));
        });
    }

    /// The camera LUT the user found, remembered by name for the next photo with it.
    pub(super) fn camera_lut_found(&mut self, photo: PathBuf, l: CameraLut, p: PathBuf) {
        let _ = settings::set_value(&camera_lut_key(&l.name), &p.to_string_lossy());
        self.input(Msg::CameraLutAdd(photo, l, p));
    }

    /// `size` at the draft scale while a development control moves.
    fn draft_size(&self, (w, h): (u32, u32)) -> (u32, u32) {
        let k = if self.drafting { self.quality.draft(self.auto_draft) } else { 1.0 };
        (((w as f32 * k) as u32).max(1), ((h as f32 * k) as u32).max(1))
    }

    /// Re-develops the RAW after a Develop change: a draft now, full quality once it settles.
    pub(super) fn develop_changed(&mut self) {
        self.drafting = self.quality.draft(self.auto_draft) < 1.0;
        self.encode_photo();
        self.request_view();
        self.timers.debounce(DEVELOP_SETTLE, Msg::DevelopSettled);
        self.schedule_save();
    }

    pub(super) fn layers_mut(&mut self, which: Layers) -> &mut Vec<LabLayer> {
        match which {
            Layers::Lab => &mut self.lab_layers,
            Layers::Look => &mut self.looks,
        }
    }

    pub(super) fn push_layer(&mut self, which: Layers, kind: LabKind, input: Profile) {
        self.layers_mut(which).push(LabLayer { kind, intensity: 1.0, input, on: true, path: None });
        self.layers_changed(which, true);
    }

    /// Reads a LUT file off the main thread and adds it to `which`.
    pub(super) fn load_layer(&self, which: Layers, path: PathBuf, known: Option<(Profile, f32)>) {
        let s = self.tx.clone();
        std::thread::spawn(move || s.send(Msg::LayerLoaded(which, path.clone(), known, Lut3d::load(&path))));
    }

    /// Adds `lut` to `which`, with its input and intensity if known.
    pub(super) fn push_lut(
        &mut self,
        which: Layers,
        lut: Arc<Lut3d>,
        path: Option<PathBuf>,
        known: Option<(Profile, f32)>,
    ) {
        // Vendors name the input in the file (e.g. "…_FLog_…"): preselect it.
        let (input, intensity) = known.unwrap_or_else(|| (Profile::guess_from_name(&lut.title), 1.0));
        self.layers_mut(which).push(LabLayer { kind: LabKind::Lut(lut), intensity, input, on: true, path });
        self.layers_changed(which, true);
    }

    /// Changes layer `i` of `which`, if there is one, and shows it.
    pub(super) fn edit_layer(&mut self, which: Layers, i: usize, f: impl FnOnce(&mut LabLayer)) {
        if let Some(l) = self.layers_mut(which).get_mut(i) {
            f(l);
            self.layers_changed(which, false);
        }
    }

    pub(super) fn layers_changed(&mut self, which: Layers, structure: bool) {
        match which {
            Layers::Lab => self.render_lab(),
            Layers::Look => {
                if structure && std::mem::take(&mut self.flash_new_look) {
                    self.ui.flash_look = self.looks.len().checked_sub(1);
                    self.timers.after(std::time::Duration::from_millis(900), Msg::FlashDone);
                }
                self.show_looks();
            }
        }
    }

    /// Sends the looks, plus the previewed candidate, to the photo and the filmstrip.
    pub(super) fn show_looks(&mut self) {
        let mut layers = self.look_layers();
        if let Some((_, lut)) = self.candidate.as_ref().filter(|_| !self.off.contains(&Off::Looks)) {
            let input = Profile::guess_from_name(&lut.title);
            layers.push(Layer { source: Source::Lut(lut.clone()), intensity: 1.0, input });
        }
        self.send(Job::SetLook(self.look_stack(layers)));
        (self.sent, self.roll_sent) = (None, None);
        self.render();
    }

    /// The candidate preview off; the selection in the grid stays.
    pub(super) fn drop_candidate(&mut self) {
        self.preview_req = None;
        if self.candidate.take().is_some() {
            self.show_looks();
        }
    }

    pub(super) fn look_layers(&self) -> Vec<Layer> {
        if self.off.contains(&Off::Looks) {
            return Vec::new();
        }
        self.looks.iter().filter(|l| l.on).map(|l| self.job_layer(l)).collect()
    }

    /// Reloads the library when files appear, change or go in its folder or sub-folders.
    pub(super) fn watch_folder(&mut self, dir: &Path) {
        use notify::{EventKind, RecursiveMode, Watcher};
        let s = self.tx.clone();
        let watcher = notify::recommended_watcher(move |e: notify::Result<notify::Event>| {
            if e.is_ok_and(|e| matches!(e.kind, EventKind::Create(_) | EventKind::Remove(_) | EventKind::Modify(_))) {
                s.send(Msg::FolderChanged);
            }
        });
        self.watcher = watcher
            .and_then(|mut w| w.watch(dir, RecursiveMode::Recursive).map(|_| w))
            .inspect_err(|e| eprintln!("rawvibes-qt: cannot watch {}: {e}", dir.display()))
            .ok();
    }

    /// The looks as the grade's output (Standard) feeds them.
    pub(super) fn look_stack(&self, layers: Vec<Layer>) -> Stack {
        Stack { input: Profile::Standard, layers, blend: self.look_blend }
    }

    /// Renders the frame the size estimate encodes (`Done::ExportFrame`).
    pub(super) fn export_frame_request(&mut self) {
        self.export_frame = None;
        self.export_gen += 1;
        self.ui.export.estimate = None;
        let mut grade = self.graded();
        if self.ui.export.format.is_some_and(PhotoFormat::hdr) {
            grade.params.hdr_stops = self.headroom;
        }
        self.send(Job::ExportFrame(grade));
    }

    /// The export dialog's file size: the preview-size frame encoded as exported, scaled by the pixel count.
    pub(super) fn export_estimate(&mut self) {
        let Some((w, h)) = self.crop_region().map(|c| Decoded::fit_size(&c, u32::MAX)) else { return };
        self.export_gen += 1;
        if self.ui.export.format == Some(PhotoFormat::Tiff16) {
            // Uncompressed: exact without encoding.
            self.ui.export.estimate = Some(format_size(w as u64 * h as u64 * 6));
            return;
        }
        self.ui.export.estimate = None;
        // A spin held down sends a value per tick: encode only the one it stops on.
        self.timers.after(std::time::Duration::from_millis(250), Msg::ExportEstimateRun(self.export_gen, w, h));
    }

    /// The zoom in effect: none on the test chart (the saved one returns with the photo).
    pub(super) fn view_zoom(&self) -> Option<Zoom> {
        self.zoom.filter(|_| !self.show_chart)
    }

    /// Full-resolution size of the open photo.
    pub fn size(&self) -> Option<(u32, u32)> {
        Some(self.decoded.as_ref()?.size())
    }

    /// The crop in full-resolution pixels of the open photo.
    pub(super) fn crop_region(&self) -> Option<Region> {
        let (w, h) = self.size()?;
        Some(crop::region(&if self.off.contains(&Off::Crop) { Crop::default() } else { self.crop }, w, h))
    }

    /// What the preview shows: the whole straightened frame while editing the crop, otherwise the crop.
    pub(super) fn shown_region(&self) -> Option<Region> {
        if self.editing.is_none() {
            return self.crop_region();
        }
        let (w, h) = self.size()?;
        Some(crop::region(&crop::frame(self.crop), w, h))
    }

    pub(super) fn restore_crop(&mut self, crop: Crop, ratio: Ratio, row: u32) {
        // A sidecar's or a paste's crop, fitted into this photo.
        self.crop = self.size().map_or(crop, |(w, h)| crop::clamp(crop, w, h));
        self.ratio = ratio;
        self.ratio_row = row;
    }

    pub(super) fn reset_crop(&mut self) {
        (self.editing, self.levelling) = (None, false);
        self.restore_crop(Crop::default(), Ratio::Free, 0);
    }

    /// A crop change: the preview, when what it shows moved.
    pub(super) fn crop_changed(&mut self) {
        if self.shown_region() != self.encoded {
            self.encode_photo();
        }
        self.schedule_save();
    }

    /// Largest crop of the current ratio at the current angle.
    pub(super) fn fit_crop(&mut self) {
        if let Some((w, h)) = self.size() {
            self.crop = crop::refit(self.crop, self.ratio, w, h);
        }
    }

    /// Re-renders the zoomed view (if any) at full resolution, off the main thread.
    pub(super) fn request_view(&mut self) {
        let (Some(z), Some(d), Some(crop)) = (self.view_zoom(), self.decoded.clone(), self.shown_region()) else {
            return;
        };
        let (r, w, h) = z.view(&crop, self.canvas);
        // Past 100% the canvas upsamples a 1:1 develop, so detail radii keep their full-resolution meaning.
        let k = (r.size[0] / w as f32).min(r.size[1] / h as f32).min(1.0);
        let (w, h) = (((w as f32 * k).round() as u32).max(1), ((h as f32 * k).round() as u32).max(1));
        self.view_gen += 1;
        let (s, generation, profile) = (self.tx.clone(), self.view_gen, self.profile);
        if d.is_raw() {
            let ((width, height), develop) = (self.draft_size((w, h)), self.applied_develop());
            return self.send(Job::DevelopRaw { generation, region: r, width, height, develop, profile, view: true });
        }
        // shortcut: one thread per request; a scroll burst resamples views nobody sees. Pool them if it shows.
        let dev = Develop::default();
        std::thread::spawn(move || {
            s.send(Msg::ViewReady(generation, profile, d.encode_region(&r, w, h, profile, &dev)))
        });
    }

    pub(super) fn set_zoom(&mut self, zoom: Option<Zoom>) {
        let was = std::mem::replace(&mut self.zoom, zoom);
        if zoom.is_some() {
            self.request_view();
        } else if was.is_some() {
            // Back to the fitted photo.
            self.view_gen += 1;
            if let Some((img, _)) = self.active_source().cloned() {
                self.send(Job::SetPhoto(img));
                self.sent = None;
                self.render();
            }
        }
    }

    /// The zoom as a percentage of the photo's pixels.
    pub fn zoom_percent(&self) -> f32 {
        let scale = match (self.zoom, self.shown_region()) {
            (Some(z), _) => z.scale,
            (None, Some(r)) => Zoom::fit_scale(&r, self.canvas),
            (None, None) => 1.0,
        };
        100.0 / scale
    }

    pub(super) fn on_zoom(&mut self, cmd: ZoomCmd) {
        let zoomable = !self.show_chart && !self.in_lab() && self.editing.is_none();
        let Some(crop) = self.shown_region().filter(|_| zoomable) else { return };
        let canvas = self.canvas;
        let middle = canvas.map(|v| v / 2.0);
        let scale = self.zoom.map_or_else(|| Zoom::fit_scale(&crop, canvas), |z| z.scale);
        let at = |s: f32, p: [f32; 2]| Zoom::zoom_at(self.zoom, s, p, &crop, canvas);
        let zoom = match cmd {
            ZoomCmd::Fit => None,
            ZoomCmd::Actual => at(1.0, middle),
            ZoomCmd::In => at(scale / std::f32::consts::SQRT_2, middle),
            ZoomCmd::Out => at(scale * std::f32::consts::SQRT_2, middle),
            // One wheel notch is a √2 step; touchpads scroll in fractions of it.
            ZoomCmd::Scroll { dy, at: p } => at(scale * 2f32.powf(dy as f32 / 2.0), p),
            ZoomCmd::Toggle(p) => self.zoom.map_or_else(|| at(1.0, p), |_| None),
            // The primary button drags the before/after split while comparing.
            ZoomCmd::Pan { button: 1, .. } if self.compare => return,
            ZoomCmd::Pan { d, .. } => self.zoom.map(|z| z.pan(d, &crop, canvas)),
        };
        if zoom != self.zoom {
            self.set_zoom(zoom);
        }
    }

    pub(super) fn on_gpu(&mut self, done: Done) {
        match done {
            Done::Failed(e) => {
                // The worker is gone: an export in progress never reports back.
                if self.exporting.take().is_some() {
                    self.ui.export.busy = false;
                }
                self.toast(&gettext!("GPU unavailable: {}", e))
            }
            Done::Graded { width, height, rgba, hdr, scopes: Scopes { histogram, waveform, vectorscope } } => {
                // The HDR rendition goes to the HDR surface; the SDR one always backs the canvas.
                if let Some(hdr) = hdr.filter(|_| self.hdr_preview) {
                    self.show_image("hdr".into(), hdr, width, height, true);
                }
                self.show_image("photo".into(), rgba, width, height, false);
                self.show_image("hist".into(), histogram, HIST_W as u32, HIST_H as u32, false);
                self.show_image("wave".into(), waveform, HIST_W as u32, HIST_H as u32, false);
                self.show_image("vec".into(), vectorscope, VEC_SIZE as u32, VEC_SIZE as u32, false);
            }
            Done::Lab { width, height, rgba } => {
                self.show_image("lab".into(), rgba, width, height, false);
            }
            Done::Thumb { generation, index, width, height, rgba } => {
                if generation == self.gallery_gen && index < self.luts.len() {
                    self.show_image(format!("thumb/{index}"), rgba, width, height, false);
                }
            }
            Done::Exported(Ok(p)) => self.toast(&gettext!("LUT saved: {}", file_label(&p))),
            Done::Exported(Err(e)) => self.toast(&gettext!("Export failed: {}", e)),
            Done::ExportProgress(f) => {
                // A batch shows its own progress: the photos done plus this one's part.
                let f = self.batch.as_ref().map_or(f, |b| ((b.total - b.left.len() - 1) as f32 + f) / b.total as f32);
                self.ui.export.progress = f;
            }
            Done::ExportFrame(frame) => {
                self.export_frame = Some(Arc::new(frame));
                self.export_estimate();
            }
            Done::PhotoExported(r) if self.batch.is_some() => {
                self.exporting = None;
                match r {
                    Ok(Some(_)) => {}
                    Ok(None) => return self.batch_stop(),
                    Err(e) => {
                        let name = self.path.as_deref().map(file_label).unwrap_or_default();
                        if let Some(b) = self.batch.as_mut() {
                            b.failed.push(format!("{name}: {e}"));
                        }
                    }
                }
                self.batch_next();
            }
            Done::PhotoExported(r) => {
                self.exporting = None;
                self.ui.export.busy = false;
                match r {
                    Ok(Some(p)) => {
                        self.ui.export.open = false;
                        self.toast(&gettext!("Photo saved: {}", file_label(&p)));
                    }
                    Ok(None) => self.toast(&gettext("Export stopped")),
                    Err(e) => self.toast(&gettext!("Export failed: {}", e)),
                }
            }
            Done::RollFrame { generation, index, width, height, rgba } => {
                if generation == self.roll_gen
                    && let Some(id) = self.roll.get(index).map(|it| it.id)
                {
                    self.show_image(format!("roll/{id}"), rgba, width, height, false);
                }
            }
            Done::RawLoaded(generation, path, r) => {
                // The strip copy always; `Loaded` drops itself when a newer open superseded it.
                let small = r.as_ref().map(|d| roll_copy(d, self.profile)).map_err(String::clone);
                self.input(Msg::RollLoaded(path.clone(), small));
                self.input(Msg::Loaded(generation, path, r));
            }
            Done::Developed { view: false, profile, photo: Some(photo), cost, .. } => {
                // Smoothed: one frame can be slow for reasons that pass.
                self.frame_cost = if self.frame_cost > 0.0 { 0.7 * self.frame_cost + 0.3 * cost } else { cost };
                if let Some(r) = self.shown_region() {
                    let (w, h) = Decoded::fit_size(&r, PREVIEW_MAX);
                    self.auto_draft = auto_draft(self.frame_cost, w * h);
                }
                self.input(Msg::Encoded(profile, photo))
            }
            Done::Developed { view: false, .. } => {}
            Done::Developed { view: true, generation, profile, .. } => {
                let shown = self.photo.as_ref().map(|p| p.1);
                if generation == self.view_gen && self.view_zoom().is_some() && shown == Some(profile) {
                    self.sent = None;
                    self.render();
                }
            }
            Done::AiProgress(g, f) if g == self.load_gen && self.develop.detail.nr_ai => {
                (self.ui.ai_fraction, self.ui.ai_busy, self.ui.ai_msg) = (f, true, None);
            }
            Done::AiReady(g) if g == self.load_gen => {
                self.ui.ai_busy = false;
                self.input(Msg::AiReady);
            }
            Done::AiFailed(g, e) if g == self.load_gen => {
                self.ui.ai_busy = false;
                self.ui.ai_msg = Some(gettext!("AI noise reduction unavailable: {}", e));
            }
            Done::AiProgress(..) | Done::AiReady(_) | Done::AiFailed(..) => {}
            Done::Sampled(c) => match self.camera().and_then(|cam| cam.neutral_for(c)) {
                Some(kt) => self.input(Msg::Develop(DevelopEdit::Picked(kt))),
                None => self.toast(&gettext("Pick a grey area that is not too dark or too bright")),
            },
            Done::Matched(generation, _) if generation != self.match_gen => {}
            Done::Matched(_, m) => {
                if let Some(path) = self.camera_job.take() {
                    self.camera_match = Some((path, m.clone()));
                }
                self.apply_match(&m);
            }
        }
    }
}

/// Display-encoded float pixels as RGBA8, for thumbnails.
fn rgba8(pixels: &[[f32; 4]]) -> Vec<u8> {
    let mut out = Vec::with_capacity(pixels.len() * 4);
    out.extend(pixels.iter().flat_map(|p| p.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)));
    out
}
