//! RawValley GTK4/libadwaita front-end over `rawvalley-core`.

/// `gettext` with `{}` placeholders filled in order (translations may not reorder them).
macro_rules! gettext {
    ($msgid:literal, $($arg:expr),+ $(,)?) => {
        $crate::fill(&gettextrs::gettext($msgid), &[$($arg.to_string()),+])
    };
}

mod crop_ui;
mod filmstrip;
mod lab_page;
mod presets_ui;
mod settings;
mod widgets;
mod zoom_ui;

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};

use crop_ui::CropUi;
use filmstrip::{Filmstrip, MAX_ROLL, ROLL_PX};
use gettextrs::gettext;
use lab_page::{LabAdd, LabKind, LabLayer, LabUi, Layers, caption};
use presets_ui::PresetsUi;
use rawvalley_core::chart::test_chart;
use rawvalley_core::crop::{self, Crop, PRESETS, Ratio};
use rawvalley_core::develop::{Camera, Develop, KELVIN, WB_PRESETS};
use rawvalley_core::gpu::{self, Done, Job, ThumbBase};
use rawvalley_core::grade::{
    BAND_HUE_RANGE, BAND_HUES, Curve, Grade, HUE_HUE, HUE_SAT, LUM_SAT, LUMA, MASTER, Params, Stage,
};
use rawvalley_core::lab::{Blend, Layer, Source, Stack, oklch_to_srgb};
use rawvalley_core::lut::{Format, Lut3d, LutEntry, MAX_LIBRARY};
use rawvalley_core::matching::{self, Match};
use rawvalley_core::photo::{Decoded, PREVIEW_MAX, Photo, RASTER_EXTS, RAW_EXTS, is_photo, is_raw};
use rawvalley_core::preset::{self, Preset};
use rawvalley_core::profiles::Profile;
use rawvalley_core::raw::{RawData, read_raw};
use rawvalley_core::scopes::{HIST_H, HIST_W, Scopes, VEC_SIZE};
use rawvalley_core::sidecar::{LookRef, Sidecar};
use rawvalley_core::view::{Region, Zoom};
use relm4::adw::prelude::*;
use relm4::gtk::{gdk, gio, glib};
use relm4::{ComponentParts, ComponentSender, RelmApp, SimpleComponent, adw, gtk};
use widgets::{ColorWheel, CurveEditor, Slider};
use zoom_ui::{ZoomCmd, ZoomUi};

const APP_ID: &str = "io.github.rawvalley.RawValley";

type Field = fn(&mut Params) -> &mut f32;

/// What an export writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum What {
    Grade,
    /// The neutral LUT (e.g. a HALD to process in another editor).
    Identity,
    Lab,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Grade,
    Lab,
}

#[derive(Debug)]
enum Msg {
    Open,
    Loaded(u64, PathBuf, Result<Decoded, String>),
    Develop(DevelopEdit),
    AsInCamera(CameraPart),
    /// The camera JPEG of a RAW, read for "As in camera".
    CameraJpeg(PathBuf, Result<Arc<Photo>, String>),
    AsInCameraOnOpen(bool),
    /// Write the sidecar if tick `u64` is still the last edit.
    SaveSidecar(u64),
    /// The window is closing: write pending edits, then close.
    Close,
    /// Look LUTs named by a sidecar, read.
    SidecarLooks(Option<PathBuf>, Vec<(LookRef, Result<Arc<Lut3d>, String>)>),
    /// A click on the canvas, in canvas device pixels.
    CanvasClick([f32; 2]),
    /// No development change for `DEVELOP_SETTLE` since tick `u64`: full quality.
    DevelopSettled(u64),
    Quality(u32),
    /// A RAW read on the CPU, ready for the GPU (load generation, file).
    RawRead(u64, PathBuf, Result<Arc<RawData>, String>),
    /// The preview encoded for a profile.
    Encoded(Profile, Arc<Photo>),
    Set(Field, f64),
    Wheel(usize, f32, f32),
    WheelLuma(usize, f64),
    Band(usize),
    Hue(usize, f64),
    Curve(usize, Curve),
    ResetAll,
    Compare(bool),
    Split(f32),
    Export(Format, usize, What),
    ExportTo(Format, usize, What, PathBuf),
    ChooseFolder,
    Folder(PathBuf),
    GalleryLoaded(Vec<LutEntry>, Vec<String>, bool),
    Group(u32),
    Search(String),
    /// Rescans the LUT folder.
    Reload,
    /// A sidebar section switched off/on (`None` = the LUTs after the grade).
    Bypass(Option<Stage>, bool),
    /// LUT browser shown or hidden.
    Browse(bool),
    /// LUT browser: preview library LUT i on the photo (None: none), its
    /// file read, the pointer resting on a card (None: off the grid) after
    /// a hover tick, add the previewed LUT to the looks, Escape.
    Preview(Option<usize>),
    PreviewLoaded(usize, Result<Arc<Lut3d>, String>),
    Hover(Option<usize>),
    HoverSettled(u64, Option<usize>),
    Commit(usize),
    Escape,
    Preferences,
    Page(Page),
    LayerAdd(Layers, LabAdd),
    OpenInLab,
    LayerLoaded(Layers, PathBuf, Result<Lut3d, String>),
    LayerRemove(Layers, usize),
    LayerMove(Layers, usize, bool),
    LayerIntensity(Layers, usize, f64),
    LayerProfile(Layers, usize, Profile),
    LabInput(Profile),
    LabBlend(Blend),
    LookBlend(Blend),
    LayerOn(Layers, usize, bool),
    /// Something changed in a watched LUT folder.
    FolderChanged,
    /// No folder change for a while since change number `u64`: reload.
    FolderSettled(u64),
    Chart(bool),
    ChartReady(Profile, Arc<Photo>),
    Profile(Profile),
    SelectLut(usize),
    Toast(String),
    /// Channel mixer: show output row / monochrome page, set a coefficient (delta from identity).
    MixerMono(bool),
    /// Channel mixer may change brightness (Preserve luminance off).
    MixerFree(bool),
    Mixer(usize, f64),
    /// Reference match: pick a file, it is decoded, then matched on the GPU.
    MatchChoose,
    MatchReference(PathBuf),
    /// The decoded reference, with its thumbnail as RGBA8.
    MatchLoaded(PathBuf, Result<(Arc<Photo>, Vec<u8>), String>),
    MatchClear,
    /// Preview aids: clipping stripes on the image, skin tone line on the vectorscope.
    Overlay(bool),
    SkinLine(bool),
    PresetsRefresh,
    PresetSave,
    PresetSaveAs(String),
    /// Writes a preset to the presets folder: `force` replaces one of the
    /// same name, `apply` loads it afterwards (imports).
    PresetWrite(Box<Preset>, bool, bool),
    PresetApply(PathBuf),
    PresetDelete(PathBuf),
    /// Puts back a deleted preset (its file and contents).
    PresetRestore(PathBuf, String),
    PresetImport,
    PresetExport,
    PresetExportTo(PathBuf),
    /// Undo of a preset load: the grade before it.
    RestoreGrade(Box<Grade>),
    /// Undo of Reset all: every edit of the photo before it.
    RestoreEdits(Box<Sidecar>),
    /// Filmstrip: add photos, a photo decoded small (already encoded for
    /// `Profile`), re-encoded for a new profile, pick or drop one, grid view.
    RollAddPaths(Vec<PathBuf>),
    RollLoaded(PathBuf, Result<(Decoded, Profile, Arc<Photo>), String>),
    RollEncoded(Profile, Vec<(PathBuf, Arc<Photo>)>),
    /// The grade settled (debounced): render the filmstrip if this is the latest tick.
    RollTick(u64),
    RollSelect(usize),
    RollRemove(usize),
    Grid(bool),
    Gpu(Done),
    Zoom(ZoomCmd),
    /// Picture area of the Grade canvas, in device pixels.
    CanvasSize([f32; 2]),
    /// A zoomed view resampled and encoded: (view generation, pixels).
    ViewReady(u64, Profile, Arc<Photo>),
    /// Crop: edit mode on/off (off applies), Escape, ratio preset index,
    /// orientation swap, straighten angle, a dragged rectangle, reset.
    CropEdit(bool),
    CropCancel,
    CropRatio(usize),
    CropSwap,
    CropAngle(f32),
    CropRect([f32; 4]),
    CropReset,
}

/// A photo of the filmstrip: its file, a small decoded copy once read, and
/// that copy encoded for the profile it was last shown in.
struct RollItem {
    path: PathBuf,
    /// The photo's own grade (its sidecar's), for its filmstrip frame.
    grade: Grade,
    small: Option<Decoded>,
    encoded: Option<(Profile, Arc<Photo>)>,
}

/// What an "As in camera" button sets from the camera JPEG and metadata.
#[derive(Debug, Clone, Copy)]
enum CameraPart {
    /// Tone: the lightness part of the match.
    Tone,
    /// Reference match: the whole match.
    Match,
    /// Every section: white balance, exposure and tones. Colours stay the
    /// camera matrix's, as RawTherapee's auto-matched tone curve: a JPEG's
    /// colours carry its Photo Style or in-camera LUT, which an a/b
    /// transfer would spread over the whole photo.
    All,
}

/// A change in the Develop group.
#[derive(Debug, Clone, Copy)]
enum DevelopEdit {
    AsShot,
    /// Dropdown index: 0 As shot, then `WB_PRESETS`, then Custom.
    Preset(u32),
    Kelvin(f32),
    Tint(f32),
    Exposure(f32),
    CameraExposure,
    Pick(bool),
    Picked([f32; 2]),
}

/// How the RAW preview trades resolution for fluidity while a development
/// control moves; the full-quality frame follows `DEVELOP_SETTLE` later.
#[derive(Clone, Copy, Debug, PartialEq)]
enum PreviewQuality {
    Fast,
    Balanced,
    Best,
}

impl PreviewQuality {
    const ALL: [PreviewQuality; 3] = [PreviewQuality::Fast, PreviewQuality::Balanced, PreviewQuality::Best];

    fn key(self) -> &'static str {
        match self {
            PreviewQuality::Fast => "fast",
            PreviewQuality::Balanced => "balanced",
            PreviewQuality::Best => "best",
        }
    }

    fn label(self) -> String {
        match self {
            PreviewQuality::Fast => gettext("Fast"),
            PreviewQuality::Balanced => gettext("Balanced"),
            PreviewQuality::Best => gettext("Best"),
        }
    }

    /// Linear scale of the preview while dragging.
    fn draft(self) -> f32 {
        match self {
            PreviewQuality::Fast => 0.5,
            PreviewQuality::Balanced => 0.75,
            PreviewQuality::Best => 1.0,
        }
    }

    fn stored() -> Self {
        let v = settings::value(settings::PREVIEW_QUALITY);
        Self::ALL.into_iter().find(|q| v.as_deref() == Some(q.key())).unwrap_or(PreviewQuality::Balanced)
    }
}

/// Quiet time after the last edit before the sidecar is written.
const SAVE_SETTLE: std::time::Duration = std::time::Duration::from_millis(1000);

/// Quiet time after the last development change before the full-quality frame.
const DEVELOP_SETTLE: std::time::Duration = std::time::Duration::from_millis(150);

/// Pointer rest on a LUT card before it is previewed.
const HOVER_REST: std::time::Duration = std::time::Duration::from_millis(120);

/// Quiet time after the last grade change before the filmstrip re-renders.
const ROLL_SETTLE: std::time::Duration = std::time::Duration::from_millis(150);

/// The filmstrip copy of a decoded photo: small, and encoded for `profile`.
fn roll_copy(d: &Decoded, profile: Profile) -> (Decoded, Profile, Arc<Photo>) {
    let small = d.downscaled(ROLL_PX);
    let photo = small.encode(profile, &Develop::default());
    (small, profile, photo)
}

/// A file's name for display.
fn file_label(path: &std::path::Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn stem_label(path: &std::path::Path) -> Option<String> {
    path.file_stem().map(|s| s.to_string_lossy().into_owned())
}

struct Ui {
    window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    grade_stack: gtk::Stack,
    grade_pic: gtk::Picture,
    info: gtk::Label,
    sidebar: Sidebar,
    gallery_stack: gtk::Stack,
    flow: gtk::FlowBox,
    folder_label: gtk::Label,
    groups: gtk::DropDown,
    prefs: adw::PreferencesDialog,
    prefs_row: adw::ActionRow,
    lab: LabUi,
    stack: adw::ViewStack,
    thumbs: Vec<gtk::Picture>,
    export: adw::SplitButton,
    chart_btn: gtk::ToggleButton,
    /// Header "As in camera" (RAWs only).
    camera_btn: gtk::Button,
    presets: PresetsUi,
    filmstrip: Filmstrip,
    /// Single photo or the grid of the filmstrip.
    canvas_stack: gtk::Stack,
    grid_btn: gtk::ToggleButton,
    legend: gtk::Revealer,
    zoom: ZoomUi,
    crop: CropUi,
}

struct App {
    grade: Grade,
    band: usize,
    compare: bool,
    split: f32,
    path: Option<PathBuf>,
    luts: Vec<LutEntry>,
    lut_paths: Arc<Vec<PathBuf>>,
    /// Sub-folder of each LUT, and the distinct ones offered by the filter
    /// (filter index 0 = all, i = group_names[i - 1]).
    group_names: Vec<String>,
    /// Gallery filters: sub-folder (None = all) and lower-cased search text.
    group: Option<String>,
    query: String,
    page: Page,
    lab_layers: Vec<LabLayer>,
    /// LUTs applied after the grade on the Grade page, baked into its export.
    looks: Vec<LabLayer>,
    /// Sections switched off with their eye toggle; they are left out of preview and export.
    bypassed: Vec<Stage>,
    looks_off: bool,
    /// The LUT browser is open (its thumbnails follow the grade).
    browsing: bool,
    /// Library LUT previewed after the looks (not part of them until
    /// committed), the latest one asked for, and the hover debounce tick.
    candidate: Option<(usize, Arc<Lut3d>)>,
    preview_req: Option<usize>,
    hover_tick: u64,
    /// The next look row built is a commit from the browser: flash it once.
    flash_new_look: bool,
    /// What the thumbnail base was last built from: grade, looks (by identity), profile.
    base_key: Option<BaseKey>,
    /// Thumbnails need re-rendering (new source or library) next time the browser opens.
    gallery_stale: bool,
    lab_input: Profile,
    lab_blend: Blend,
    look_blend: Blend,
    /// Watch the LUT folder and its sub-folders; `folder_changes` debounces their events.
    monitors: Vec<gio::FileMonitor>,
    folder_changes: u64,
    profile: Profile,
    /// The open file, decoded once; `photo` is it encoded for a profile.
    decoded: Option<Decoded>,
    /// Shown source and the profile its pixels are encoded in; the test
    /// chart can be swapped in and out.
    photo: Option<(Arc<Photo>, Profile)>,
    chart: Option<(Arc<Photo>, Profile)>,
    show_chart: bool,
    selected: Option<usize>,
    jobs: mpsc::Sender<Job>,
    /// Last preview request; identical requests (e.g. echoes of programmatic
    /// widget resets) are not re-rendered.
    sent: Option<(Grade, f32)>,
    /// Latest photo load / gallery pass; results from older ones are dropped.
    load_gen: u64,
    gallery_gen: u64,
    /// Clipping stripes on the Grade preview.
    overlay: bool,
    /// Filmstrip photos and the one being edited; `roll_gen` tags GPU frames.
    roll: Vec<RollItem>,
    active: Option<usize>,
    roll_gen: u64,
    /// Grade the filmstrip was last rendered with (None: render on the next
    /// change), and the latest debounce tick.
    roll_sent: Option<Vec<Grade>>,
    roll_tick: u64,
    /// The match panel shows the reference file (false: matched by a preset, or no match).
    match_file: bool,
    /// Latest match request; results of older ones are dropped.
    match_gen: u64,
    /// The component's own input, for timers and worker threads.
    input: relm4::Sender<Msg>,
    crop: Crop,
    ratio: Ratio,
    /// While editing the crop: the crop, ratio and ratio row before editing (Escape restores them).
    editing: Option<(Crop, Ratio, u32)>,
    /// Grade canvas zoom (None: fitted), its picture area in device pixels,
    /// and the latest zoomed view request (older results are dropped).
    zoom: Option<Zoom>,
    canvas: [f32; 2],
    view_gen: u64,
    /// Preview encodes run one at a time; a request made meanwhile re-runs
    /// once the current one lands (whose result is then stale). `encoded`
    /// is the region last asked for.
    encoding: bool,
    /// What the user set in the Develop group (RAWs only).
    develop: Develop,
    quality: PreviewQuality,
    /// A development control is moving: RAW views render at the draft scale.
    drafting: bool,
    develop_tick: u64,
    /// The camera match of a file, once computed.
    camera_match: Option<(PathBuf, Match)>,
    /// The match in flight is the camera's, for this file.
    camera_job: Option<PathBuf>,
    /// Which parts (lightness, colour) the match in flight loads.
    match_parts: (bool, bool),
    /// The grade before a section's "As in camera", for the Undo toast.
    match_undo: Option<Grade>,
    /// The open photo's edits as last read from or written to its sidecar.
    saved: Option<Sidecar>,
    save_tick: u64,
    /// Sidecar looks still loading: nothing is saved meanwhile.
    looks_loading: bool,
    /// A sidecar write failed this session (reported once).
    save_failed: bool,
    encode_again: bool,
    encoded: Option<Region>,
    ui: Ui,
}

impl App {
    fn split_value(&self) -> f32 {
        if self.compare { self.split } else { 0.0 }
    }

    fn send(&self, job: Job) {
        if self.jobs.send(job).is_err() {
            self.toast(&gettext("GPU unavailable"));
        }
    }

    fn active(&self) -> Option<&(Arc<Photo>, Profile)> {
        if self.show_chart { self.chart.as_ref() } else { self.photo.as_ref() }
    }

    fn has_source(&self) -> bool {
        self.active().is_some()
    }

    /// Profile of the pixels currently shown (photo or chart).
    fn source_profile(&self) -> Profile {
        self.active().map_or(Profile::Standard, |s| s.1)
    }

    /// Sends the active source (photo or test chart) to the GPU and redraws.
    fn show_source(&mut self) {
        let Some((img, profile)) = self.active().cloned() else {
            // Nothing to show (e.g. chart turned off with no photo open).
            self.ui.export.set_sensitive(false);
            self.ui.info.set_label("");
            self.ui.grade_stack.set_visible_child_name("empty");
            return;
        };
        // The shader decodes the profile the pixels were actually encoded in.
        self.grade.params.set_input(profile);
        let name = match (&self.path, self.show_chart) {
            (Some(p), false) => p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            _ => gettext("Test chart"),
        };
        let [w, h] = match (self.crop_region(), self.show_chart) {
            (Some(r), false) => r.size.map(|v| v.round() as u32),
            _ => [img.width, img.height],
        };
        self.ui.info.set_label(&format!("{name} · {} · {w}×{h}", profile.name()));
        self.ui.zoom.tools.set_visible(!self.show_chart);
        self.zoom_label();
        if self.view_zoom().is_some() {
            // Zoomed: the GPU keeps the zoomed view until its replacement is ready (no flash of the fit).
            self.request_view();
        } else {
            self.send(Job::SetPhoto(img));
            self.sent = None;
        }
        self.ui.export.set_sensitive(true);
        self.ui.grade_stack.set_visible_child_name("image");
        self.gallery_stale = true;
        self.refresh();
    }

    /// The grade as previewed and exported: switched-off sections are neutral.
    fn graded(&self) -> Grade {
        let mut g = self.grade.clone();
        self.bypassed.iter().for_each(|&s| g.bypass(s));
        g
    }

    fn render(&mut self) {
        let mut grade = self.graded();
        grade.params.overlay = self.overlay as u32;
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
    fn sidecar_state(&self) -> Sidecar {
        let looks = self
            .looks
            .iter()
            .filter_map(|l| {
                let path = l.path.clone()?;
                Some(LookRef { path, intensity: l.intensity, input: l.input, on: l.on })
            })
            .collect();
        let (grade, look_blend) = (self.grade.clone(), self.look_blend);
        Sidecar { develop: self.develop, grade, crop: self.crop, ratio: self.ratio, looks, look_blend }
    }

    /// Saves the sidecar a second after the last edit.
    fn schedule_save(&mut self) {
        if self.saved.is_none() {
            return;
        }
        self.save_tick += 1;
        let (s, tick) = (self.input.clone(), self.save_tick);
        glib::timeout_add_local_once(SAVE_SETTLE, move || s.emit(Msg::SaveSidecar(tick)));
    }

    /// Writes the open photo's sidecar if its edits changed since it was read or written.
    fn save_now(&mut self) {
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
    fn restore_sidecar(&mut self, sc: Sidecar, sender: &ComponentSender<Self>) {
        self.develop = sc.develop;
        self.sync_develop();
        let row = crop::PRESETS
            .iter()
            .position(|(_, r)| {
                *r == sc.ratio
                    || matches!((r, sc.ratio), (Ratio::Fixed(a, b), Ratio::Fixed(c, d)) if (*a, *b) == (d, c))
            })
            .unwrap_or(0);
        self.restore_crop(sc.crop, sc.ratio, row as u32);
        {
            self.apply_grade(sc.grade);
            self.ui.sidebar.look_blend_toggle.set_active_name(Some(if sc.look_blend == Blend::Mix {
                "mix"
            } else {
                "chain"
            }));
            if !sc.looks.is_empty() {
                self.looks_loading = true;
                let (s, refs, path) = (sender.input_sender().clone(), sc.looks, self.path.clone());
                std::thread::spawn(move || {
                    let loaded = refs.into_iter().map(|r| {
                        let lut = Lut3d::load(&r.path).map(Arc::new);
                        (r, lut)
                    });
                    s.emit(Msg::SidecarLooks(path, loaded.collect()));
                });
            }
        }
    }

    /// Each filmstrip frame's grade: the open photo's as edited, the others'
    /// from their sidecars, in the current input profile.
    fn roll_grades(&self) -> Vec<Grade> {
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

    /// The filmstrip through each frame's grade (the open photo's look LUTs,
    /// applied by the worker, on its frame only). Overlay and split do not touch it, so only grade changes
    /// count, and only once a drag settles: the main preview never waits.
    fn render_roll(&mut self) {
        // The open LUT browser's thumbnails also follow the settled grade.
        let roll_current = self.roll.is_empty() || self.roll_sent.as_ref() == Some(&self.roll_grades());
        if roll_current && !self.browsing {
            return;
        }
        self.roll_tick += 1;
        let (s, tick) = (self.input.clone(), self.roll_tick);
        glib::timeout_add_local_once(ROLL_SETTLE, move || s.emit(Msg::RollTick(tick)));
    }

    /// Hands the filmstrip to the GPU; `layout` when photos were added or
    /// removed (frame indices changed). Not yet decoded: a black frame.
    fn upload_roll(&mut self, layout: bool) {
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
    fn reencode_roll(&self) {
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
        let s = self.input.clone();
        std::thread::spawn(move || {
            let encoded = stale.into_iter().map(|(p, small)| (p, small.encode(profile, &Develop::default()))).collect();
            s.emit(Msg::RollEncoded(profile, encoded));
        });
    }

    /// Takes photo `i` out of the strip; true if it was the one being edited.
    fn drop_from_roll(&mut self, i: usize, sender: &ComponentSender<Self>) -> bool {
        self.roll.remove(i);
        let was_active = self.active == Some(i);
        self.active = self.active.and_then(|a| if a > i { Some(a - 1) } else { (a < i).then_some(a) });
        self.roll_changed(sender);
        was_active
    }

    /// Shows the match panel as the grade has it.
    fn show_match_panel(&self) {
        let matched = self.grade.params.match_amount > 0.0;
        self.ui.sidebar.matching.stack.set_visible_child_name(if matched { "set" } else { "empty" });
    }

    /// Drops any in-flight match.
    fn cancel_match(&mut self) {
        self.match_gen += 1;
        self.show_match_panel();
    }

    /// Matches the photo on screen (as decoded, before any grading) to
    /// `reference`, from its small filmstrip copy when there is one.
    fn request_match(&mut self, reference: Arc<Photo>) {
        let small = self.active.and_then(|i| self.roll.get(i)).and_then(|it| it.encoded.clone());
        let source = match (small, self.active()) {
            (Some((profile, photo)), Some((_, shown))) if profile == *shown && !self.show_chart => photo,
            (_, Some((photo, _))) => photo.clone(),
            (_, None) => return self.toast(&no_photo_to_match()),
        };
        (self.match_parts, self.camera_job) = ((true, true), None);
        self.match_gen += 1;
        self.ui.sidebar.matching.stack.set_visible_child_name("busy");
        self.send(Job::Match { generation: self.match_gen, source, reference, input: self.grade.neutralized() });
    }

    /// Rebuilds the filmstrip cards after photos were added or removed.
    fn roll_changed(&mut self, sender: &ComponentSender<Self>) {
        let names: Vec<String> = self.roll.iter().map(|it| file_label(&it.path)).collect();
        self.ui.filmstrip.rebuild(&names, self.active, sender);
        self.ui.grid_btn.set_sensitive(self.roll.len() > 1);
        if self.roll.len() < 2 && self.ui.grid_btn.is_active() {
            self.ui.grid_btn.set_active(false);
        }
        self.upload_roll(true);
    }

    /// Opens filmstrip photo `i` for editing (full-size decode off the main thread).
    fn load_active(&mut self, i: usize, sender: &ComponentSender<Self>) {
        let Some(path) = self.roll.get(i).map(|it| it.path.clone()) else { return };
        self.active = Some(i);
        self.ui.filmstrip.set_active(self.active);
        // ponytail: a failed save only loses the dialog's start folder.
        if let Some(dir) = path.parent() {
            let _ = settings::set_folder(settings::PHOTO_FOLDER, dir);
        }
        self.ui.grade_stack.set_visible_child_name(if self.has_source() { "image" } else { "loading" });
        self.ui.info.set_label(&gettext("Loading…"));
        self.load_gen += 1;
        let (s, generation, profile) = (sender.input_sender().clone(), self.load_gen, self.profile);
        std::thread::spawn(move || {
            // One full-resolution decode at a time: each peaks above 1 GB on a 24 MP RAW.
            static FULL_DECODE: std::sync::Mutex<()> = std::sync::Mutex::new(());
            let _one = FULL_DECODE.lock().unwrap_or_else(|e| e.into_inner());
            if is_raw(&path) {
                // Developed on the GPU: the strip copy comes back with it (Done::RawLoaded).
                return s.emit(Msg::RawRead(generation, path.clone(), read_raw(&path).map(Arc::new)));
            }
            let r = Decoded::load(&path);
            // The strip copy comes from this decode (RollAddPaths skips it), even if this load is superseded.
            let small = r.as_ref().map(|d| roll_copy(d, profile)).map_err(String::clone);
            s.emit(Msg::RollLoaded(path.clone(), small));
            s.emit(Msg::Loaded(generation, path, r));
        });
    }

    /// Replaces the grade (preset, undo, reset) and shows it in every control.
    /// The input decoding stays: it belongs to the photo, not the grade.
    fn apply_grade(&mut self, mut grade: Grade) {
        (grade.params.input, grade.params.gamut) = (self.grade.params.input, self.grade.params.gamut);
        self.grade = grade;
        self.cancel_match();
        let p = self.grade.params;
        let sb = &self.ui.sidebar;
        // Each control echoes its value back as a message; the values already
        // match the grade, so the echoes change nothing.
        for (s, field) in &sb.sliders {
            s.adj.set_value(*field(&mut p.clone()) as f64);
        }
        for (i, (w, luma)) in sb.wheels.iter().enumerate() {
            let v = *wheel_mut(&mut p.clone(), i);
            w.set(v[0] as f64, v[1] as f64);
            luma.adj.set_value(v[2] as f64);
        }
        sb.curves.set_all(&self.grade.curves);
        sb.hue_curves.set_all(&self.grade.curves);
        sb.mixer.mono.set_active(p.mixer_mono > 0.5);
        sb.mixer.keep.set_active(p.mixer_free < 0.5);
        for (s, i, base) in &sb.mixer.sliders {
            s.adj.set_value(base + mixer_value(&p, *i) as f64);
        }
        sb.matching.amount.adj.set_value(p.match_amount as f64);
        if p.match_amount <= 0.0 {
            self.match_file = false;
        }
        if !self.match_file {
            sb.matching.pic.set_paintable(None::<&gdk::Paintable>);
            sb.matching.name.set_label(&gettext("Matched by a preset"));
        }
        self.show_band(self.band);
        self.sent = None;
        self.render();
    }

    /// Points the three selective-colour sliders at band `b`.
    fn show_band(&mut self, b: usize) {
        self.band = b;
        // The resulting Hue echoes carry unchanged values, so render() skips them.
        let p = self.grade.params;
        for (k, (s, v)) in self.ui.sidebar.hue.iter().zip([p.hue_shift[b], p.hue_sat[b], p.hue_lum[b]]).enumerate() {
            for i in 0..BAND_HUES.len() {
                s.scale.remove_css_class(&format!("{}-{i}", BAND_TRACKS[k]));
            }
            s.scale.add_css_class(&format!("{}-{b}", BAND_TRACKS[k]));
            s.adj.set_value(v as f64);
        }
    }

    fn presets(&self) -> Vec<(Preset, PathBuf)> {
        preset::list(&settings::presets_dir())
    }

    /// Toast with an Undo button that sends `undo()`.
    fn undo_toast(&self, title: &str, undo: impl Fn() -> Msg + 'static) {
        let toast = adw::Toast::builder().title(title).button_label(gettext("Undo")).timeout(6).build();
        let s = self.input.clone();
        toast.connect_button_clicked(move |_| s.emit(undo()));
        self.ui.toasts.add_toast(toast);
    }

    /// The grade as seen (switched-off sections stay off), as a preset.
    fn current_preset(&self, name: String) -> Preset {
        Preset { name, grade: self.graded().neutral_input() }
    }

    /// A selected LUT the filters now hide is no longer previewed.
    fn hide_filtered_selection(&mut self) {
        let hidden = self.selected.and_then(|i| self.ui.flow.child_at_index(i as i32)).is_some_and(|c| !c.is_visible());
        if hidden {
            self.clear_selection();
        }
    }

    /// No card selected and nothing previewed.
    fn clear_selection(&mut self) {
        self.ui.flow.unselect_all();
        self.selected = None;
        self.drop_candidate();
    }

    fn filter_gallery(&self) {
        for (i, l) in self.luts.iter().enumerate() {
            if let Some(c) = self.ui.flow.child_at_index(i as i32) {
                let in_group = self.group.as_ref().is_none_or(|g| *g == l.group);
                let found = self.query.is_empty()
                    || l.title.to_lowercase().contains(&self.query)
                    || l.group.to_lowercase().contains(&self.query);
                c.set_visible(in_group && found);
            }
        }
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
        // ponytail: an untouched grade keeps the exact direct path (no lattice round trip).
        let base = (!grade.is_neutral() || !looks.is_empty()).then(|| ThumbBase {
            grade,
            looks: self.look_stack(looks),
            input,
        });
        self.send(Job::SetThumbBase(base));
        true
    }

    /// Thumbnails, when the browser is open and what they show changed.
    fn refresh_gallery(&mut self) {
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

    /// Renders only the view on screen; the others refresh when shown.
    fn refresh(&mut self) {
        match self.page {
            Page::Grade => self.render(),
            Page::Lab => self.render_lab(),
        }
    }

    fn lab_job_layers(&self) -> Vec<Layer> {
        self.lab_layers.iter().filter(|l| l.on).map(|l| self.job_layer(l)).collect()
    }

    fn job_layer(&self, l: &LabLayer) -> Layer {
        let (source, input) = match &l.kind {
            LabKind::Lut(lut) => (Source::Lut(lut.clone()), l.input),
            LabKind::Grade => (Source::Grade(Box::new(self.graded())), self.source_profile()),
            LabKind::Convert(p) => (Source::Convert(*p), *p),
        };
        Layer { source, intensity: l.intensity, input }
    }

    fn lab_stack(&self, input: Profile) -> Stack {
        Stack { input, layers: self.lab_job_layers(), blend: self.lab_blend }
    }

    fn render_lab(&self) {
        if self.has_source() && !self.lab_layers.is_empty() {
            // The preview shows the stack on the current source, so its input is the source's profile.
            self.send(Job::Lab { stack: self.lab_stack(self.source_profile()), split: self.split_value() });
        }
    }

    /// The open file's crop fitted to `PREVIEW_MAX` and expressed in the
    /// current profile, off the main thread (rasters are already a signal).
    fn encode_photo(&mut self, sender: &ComponentSender<Self>) {
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
            let develop = self.develop;
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
        let s = sender.input_sender().clone();
        std::thread::spawn(move || {
            s.emit(Msg::Encoded(profile, d.encode_fit(&r, PREVIEW_MAX, profile, &Develop::default())))
        });
    }

    fn camera(&self) -> Option<&Camera> {
        self.decoded.as_ref()?.camera()
    }

    /// Applies a Develop edit; re-develops only when something changed.
    fn edit_develop(&mut self, edit: DevelopEdit, sender: &ComponentSender<Self>) {
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
            DevelopEdit::Picked(kt) => {
                dev.wb = Some(kt);
                self.ui.sidebar.develop.picker.set_active(false);
            }
            DevelopEdit::Pick(on) => {
                self.ui.grade_pic.set_cursor_from_name(on.then_some("crosshair"));
                return;
            }
        }
        if dev != self.develop {
            self.develop = dev;
            self.sync_develop();
            self.develop_changed(sender);
        }
    }

    /// The Develop controls as `self.develop` has them.
    fn sync_develop(&self) {
        let raw = self.camera().is_some();
        self.ui.camera_btn.set_sensitive(raw);
        self.ui.sidebar.matching.camera.set_visible(raw);
        self.ui.sidebar.camera_btn.set_sensitive(raw);
        let ui = &self.ui.sidebar.develop;
        ui.root.set_visible(raw);
        let Some(cam) = self.camera() else { return };
        let [k, t] = self.develop.wb.unwrap_or_else(|| cam.as_shot_kelvin_tint());
        let preset = match self.develop.wb {
            None => 0,
            Some(kt) => WB_PRESETS.iter().position(|&(_, k, t)| [k, t] == kt).map_or(WB_CUSTOM, |i| i as u32 + 1),
        };
        ui.syncing.set(true);
        ui.kelvin.adj.set_value(k as f64);
        ui.tint.adj.set_value(t as f64);
        ui.exposure.adj.set_value(self.develop.exposure as f64);
        ui.preset.set_selected(preset);
        ui.syncing.set(false);
    }

    /// The point of the oriented full-resolution image under a canvas point.
    fn image_point(&self, p: [f32; 2]) -> Option<[f32; 2]> {
        let crop = self.shown_region()?;
        let (r, scale) = match self.view_zoom() {
            Some(z) => (z.view(&crop, self.canvas).0, z.scale),
            None => (crop, Zoom::fit_scale(&crop, self.canvas)),
        };
        Some(r.point([(p[0] - self.canvas[0] / 2.0) * scale, (p[1] - self.canvas[1] / 2.0) * scale]))
    }

    /// Loads parts of `m` into the grade and shows the match panel as set.
    fn apply_match(&mut self, m: &Match, (lightness, colour): (bool, bool)) {
        matching::apply(&mut self.grade, m, lightness, colour);
        // The match lives in the collapsed Reference match section: undo it from here.
        if let Some(before) = self.match_undo.take() {
            self.undo_toast(&gettext("Camera tones applied"), move || Msg::RestoreGrade(Box::new(before.clone())));
        }
        let ui = &self.ui.sidebar.matching;
        ui.amount.adj.set_value(100.0);
        ui.stack.set_visible_child_name("set");
        self.render();
    }

    /// "As in camera": WB and exposure from the metadata (`All`), tones (and
    /// for `Match` colours) from a match to the camera JPEG, computed once per file.
    fn as_in_camera(&mut self, part: CameraPart, sender: &ComponentSender<Self>) {
        let (Some(path), true) = (self.path.clone(), self.camera().is_some()) else { return };
        if matches!(part, CameraPart::All) {
            self.edit_develop(DevelopEdit::AsShot, sender);
            self.edit_develop(DevelopEdit::CameraExposure, sender);
            // A colour match left from before would survive a tones-only load.
            self.grade.params.match_ab = Params::default().match_ab;
        }
        // Not for `All`, which also runs on every RAW opened as in camera.
        self.match_undo = matches!(part, CameraPart::Tone).then(|| self.grade.clone());
        let parts = match part {
            CameraPart::Tone | CameraPart::All => (true, false),
            CameraPart::Match => (true, true),
        };
        if let Some((_, m)) = self.camera_match.clone().filter(|c| c.0 == path) {
            return self.apply_match(&m, parts);
        }
        self.ui.sidebar.matching.stack.set_visible_child_name("busy");
        let s = sender.input_sender().clone();
        std::thread::spawn(move || {
            let r = rawvalley_core::photo::embedded_preview(&path).map(Arc::new);
            s.emit(Msg::CameraJpeg(path, r));
        });
        self.match_parts = parts;
    }

    /// `size` at the draft scale while a development control moves.
    fn draft_size(&self, (w, h): (u32, u32)) -> (u32, u32) {
        let k = if self.drafting { self.quality.draft() } else { 1.0 };
        (((w as f32 * k) as u32).max(1), ((h as f32 * k) as u32).max(1))
    }

    /// Re-develops the RAW after a Develop change: a draft now, full quality once it settles.
    fn develop_changed(&mut self, sender: &ComponentSender<Self>) {
        self.drafting = self.quality != PreviewQuality::Best;
        self.encode_photo(sender);
        self.request_view();
        self.develop_tick += 1;
        let (s, tick) = (self.input.clone(), self.develop_tick);
        glib::timeout_add_local_once(DEVELOP_SETTLE, move || s.emit(Msg::DevelopSettled(tick)));
    }

    fn layers_mut(&mut self, which: Layers) -> &mut Vec<LabLayer> {
        match which {
            Layers::Lab => &mut self.lab_layers,
            Layers::Look => &mut self.looks,
        }
    }

    fn push_layer(&mut self, which: Layers, kind: LabKind, input: Profile, sender: &ComponentSender<Self>) {
        self.layers_mut(which).push(LabLayer { kind, intensity: 1.0, input, on: true, path: None });
        self.layers_changed(which, true, sender);
    }

    /// Reads a LUT file off the main thread and adds it to `which`.
    fn load_layer(which: Layers, path: PathBuf, sender: &ComponentSender<Self>) {
        let s = sender.input_sender().clone();
        std::thread::spawn(move || s.emit(Msg::LayerLoaded(which, path.clone(), Lut3d::load(&path))));
    }

    fn push_lut(&mut self, which: Layers, lut: Arc<Lut3d>, path: Option<PathBuf>, sender: &ComponentSender<Self>) {
        // Vendors name the input in the file (e.g. "…_FLog_…"): preselect it.
        let input = Profile::guess_from_name(&lut.title);
        self.layers_mut(which).push(LabLayer { kind: LabKind::Lut(lut), intensity: 1.0, input, on: true, path });
        self.layers_changed(which, true, sender);
    }

    fn layers_changed(&mut self, which: Layers, structure: bool, sender: &ComponentSender<Self>) {
        match which {
            Layers::Lab => {
                if !structure {
                    self.ui.lab.refresh_summary(&self.lab_layers, self.lab_blend, self.lab_input);
                }
                self.lab_changed(structure, sender);
            }
            Layers::Look => {
                if structure {
                    lab_page::layer_rows(
                        &self.ui.sidebar.looks,
                        &self.looks,
                        Layers::Look,
                        self.source_profile(),
                        sender,
                    );
                    // Shown with two looks, and while Mix is on so it can be switched back.
                    self.ui.sidebar.look_blend.set_visible(self.looks.len() > 1 || self.look_blend == Blend::Mix);
                    // Show where a LUT added from the browser went.
                    let last = self.ui.sidebar.looks.row_at_index(self.looks.len() as i32 - 1);
                    if let Some(row) = last.filter(|_| std::mem::take(&mut self.flash_new_look)) {
                        row.add_css_class("flash");
                        glib::timeout_add_local_once(std::time::Duration::from_millis(900), move || {
                            row.remove_css_class("flash")
                        });
                    }
                }
                self.show_looks();
            }
        }
    }

    /// Sends the looks, plus the previewed candidate, to the photo and the filmstrip.
    fn show_looks(&mut self) {
        let mut layers = self.look_layers();
        if let Some((_, lut)) = self.candidate.as_ref().filter(|_| !self.looks_off) {
            let input = Profile::guess_from_name(&lut.title);
            layers.push(Layer { source: Source::Lut(lut.clone()), intensity: 1.0, input });
        }
        self.send(Job::SetLook(self.look_stack(layers)));
        (self.sent, self.roll_sent) = (None, None);
        self.render();
    }

    /// The candidate preview off; the selection in the grid stays.
    fn drop_candidate(&mut self) {
        self.preview_req = None;
        if self.candidate.take().is_some() {
            self.show_looks();
        }
    }

    fn look_layers(&self) -> Vec<Layer> {
        if self.looks_off {
            return Vec::new();
        }
        self.looks.iter().filter(|l| l.on).map(|l| self.job_layer(l)).collect()
    }

    /// Reloads the library when files appear, change or go in its folder or
    /// sub-folders (a new sub-folder is seen by its parent, then watched).
    fn watch_folders(&mut self, sender: &ComponentSender<Self>) {
        // Every folder, empty ones too, so LUTs copied into a new sub-folder show up.
        let mut dirs: Vec<PathBuf> = settings::folder(settings::LUT_FOLDER).into_iter().collect();
        let mut i = 0;
        while let Some(d) = dirs.get(i).cloned() {
            i += 1;
            let subs = std::fs::read_dir(&d).into_iter().flatten().flatten();
            // file_type() does not follow symlinks, so a link loop cannot recurse forever.
            dirs.extend(subs.filter(|e| e.file_type().is_ok_and(|t| t.is_dir())).map(|e| e.path()));
        }
        self.monitors = dirs
            .iter()
            .filter_map(|d| {
                let m = gio::File::for_path(d)
                    .monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
                    .ok()?;
                let s = sender.input_sender().clone();
                m.connect_changed(move |_, _, _, event| {
                    use gio::FileMonitorEvent as E;
                    if matches!(
                        event,
                        E::Created | E::Deleted | E::MovedIn | E::MovedOut | E::Renamed | E::ChangesDoneHint
                    ) {
                        s.emit(Msg::FolderChanged);
                    }
                });
                Some(m)
            })
            .collect();
    }

    /// The looks as the grade's output (Standard) feeds them.
    fn look_stack(&self, layers: Vec<Layer>) -> Stack {
        Stack { input: Profile::Standard, layers, blend: self.look_blend }
    }

    fn lab_changed(&mut self, structure: bool, sender: &ComponentSender<Self>) {
        if structure {
            self.ui.lab.rebuild(&self.lab_layers, self.lab_blend, self.lab_input, self.source_profile(), sender);
        }
        self.render_lab();
    }

    fn toast(&self, text: &str) {
        self.ui.toasts.add_toast(adw::Toast::builder().title(text).timeout(4).build());
    }
}

fn texture(width: u32, height: u32, rgba: Vec<u8>) -> gdk::MemoryTexture {
    let bytes = glib::Bytes::from_owned(rgba);
    gdk::MemoryTexture::new(width as i32, height as i32, gdk::MemoryFormat::R8g8b8a8, &bytes, width as usize * 4)
}

/// Card with a collapsible header, like the sections of a grading panel.
fn section(title: &str, child: &impl IsA<gtk::Widget>) -> gtk::Box {
    section_with(title, child, &[], true)
}

/// Section with an eye toggle that switches it off (`which` = None: the LUTs
/// after the grade). Advanced sections start `open: false`, collapsed.
fn bypassable(
    title: &str,
    child: &impl IsA<gtk::Widget>,
    which: Option<Stage>,
    open: bool,
    sender: &ComponentSender<App>,
) -> gtk::Box {
    bypassable_with(title, child, which, open, &[], sender)
}

/// [`bypassable`] with `extra` header buttons before the eye (e.g. "As in camera").
fn bypassable_with(
    title: &str,
    child: &impl IsA<gtk::Widget>,
    which: Option<Stage>,
    open: bool,
    extra: &[gtk::Widget],
    sender: &ComponentSender<App>,
) -> gtk::Box {
    let eye = gtk::ToggleButton::builder()
        .icon_name("view-reveal-symbolic")
        .tooltip_text(gettext("Disable section"))
        .valign(gtk::Align::Center)
        .css_classes(["flat", "circular"])
        .build();
    let (snd, content) = (sender.clone(), child.clone().upcast::<gtk::Widget>());
    eye.connect_toggled(move |b| {
        let off = b.is_active();
        b.set_icon_name(if off { "view-conceal-symbolic" } else { "view-reveal-symbolic" });
        b.set_tooltip_text(Some(&if off { gettext("Enable section") } else { gettext("Disable section") }));
        if off {
            content.add_css_class("bypassed")
        } else {
            content.remove_css_class("bypassed")
        }
        snd.input(Msg::Bypass(which, off));
    });
    let mut header: Vec<gtk::Widget> = extra.to_vec();
    header.push(eye.upcast());
    section_with(title, child, &header, open)
}

/// "As in camera" button for a section header.
fn camera_button(tip: &str, msg: fn() -> Msg, sender: &ComponentSender<App>) -> gtk::Button {
    let b = gtk::Button::builder()
        .icon_name("camera-photo-symbolic")
        .tooltip_text(tip)
        .valign(gtk::Align::Center)
        .css_classes(["flat", "circular"])
        .build();
    b.update_property(&[gtk::accessible::Property::Label(tip)]);
    let snd = sender.clone();
    b.connect_clicked(move |_| snd.input(msg()));
    b
}

/// Heading of a sidebar group (Develop, Grade, Finish).
fn group_heading(text: &str) -> gtk::Label {
    let l = gtk::Label::builder().label(text).xalign(0.0).css_classes(["caption-heading", "dim-label"]).build();
    l.set_margin_top(6);
    l.set_margin_start(6);
    l
}

fn section_with(title: &str, child: &impl IsA<gtk::Widget>, header: &[gtk::Widget], open: bool) -> gtk::Box {
    let arrow = gtk::Image::from_icon_name(if open { "pan-down-symbolic" } else { "pan-end-symbolic" });
    let head = gtk::Box::builder().spacing(6).build();
    head.append(&gtk::Label::builder().label(title).css_classes(["heading"]).xalign(0.0).hexpand(true).build());
    head.append(&arrow);
    let button = gtk::Button::builder().child(&head).css_classes(["flat", "section-header"]).build();
    button.update_property(&[gtk::accessible::Property::Label(title)]);
    let revealer = gtk::Revealer::builder()
        .child(child)
        .reveal_child(open)
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .build();
    let card = gtk::Box::builder().orientation(gtk::Orientation::Vertical).css_classes(["card", "section"]).build();
    if !open {
        card.add_css_class("collapsed");
    }
    let (r, c) = (revealer.clone(), card.clone());
    button.update_state(&[gtk::accessible::State::Expanded(Some(open))]);
    button.connect_clicked(move |b| {
        let open = !r.reveals_child();
        b.update_state(&[gtk::accessible::State::Expanded(Some(open))]);
        r.set_reveal_child(open);
        // Symmetric padding once the body is gone, so the header sits centred.
        if open {
            c.remove_css_class("collapsed")
        } else {
            c.add_css_class("collapsed")
        }
        arrow.set_icon_name(Some(if open { "pan-down-symbolic" } else { "pan-end-symbolic" }));
    });
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    button.set_hexpand(true);
    top.append(&button);
    for (i, w) in header.iter().enumerate() {
        if i + 1 == header.len() {
            w.set_margin_end(6);
        }
        top.append(w);
    }
    card.append(&top);
    card.append(&revealer);
    card
}

/// Where a file dialog opens: the remembered folder for `key`, if it still exists.
fn start_in(key: &str) -> Option<gio::File> {
    settings::folder(key).map(gio::File::for_path)
}

fn vbox(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Vertical, spacing)
}

/// Segmented control (libadwaita ≥ 1.7) returning the group.
fn toggles(names: &[(&str, &str)]) -> adw::ToggleGroup {
    let group = adw::ToggleGroup::builder().css_classes(["round"]).homogeneous(true).build();
    for (name, label) in names {
        group.add(adw::Toggle::builder().name(*name).label(*label).build());
    }
    group
}

fn file_filters(name: &str, patterns: &[String]) -> gio::ListStore {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some(name));
    for p in patterns {
        filter.add_suffix(p);
    }
    let store = gio::ListStore::new::<gtk::FileFilter>();
    store.append(&filter);
    store
}

/// Fraction of the displayed image under widget x (Picture uses `contain`).
fn image_fraction(pic: &gtk::Picture, x: f64) -> f32 {
    let Some(p) = pic.paintable() else { return 0.5 };
    let (w, h) = (pic.width() as f64, pic.height() as f64);
    let aspect = p.intrinsic_aspect_ratio();
    let shown = if aspect > 0.0 { (h * aspect).min(w) } else { w };
    (((x - (w - shown) / 2.0) / shown).clamp(0.001, 0.999)) as f32
}

fn add_split_drag(pic: &gtk::Picture, sender: &ComponentSender<App>) {
    let drag = gtk::GestureDrag::new();
    let (p, s) = (pic.clone(), sender.clone());
    drag.connect_drag_begin(move |_, x, _| s.input(Msg::Split(image_fraction(&p, x))));
    let (p, s) = (pic.clone(), sender.clone());
    drag.connect_drag_update(move |g, dx, _| {
        if let Some((x, _)) = g.start_point() {
            s.input(Msg::Split(image_fraction(&p, x + dx)));
        }
    });
    pic.add_controller(drag);
}

/// Space around the photo on its canvas (`.canvas picture` margin); the
/// zoom and crop overlays map pointer positions through it.
pub const CANVAS_MARGIN: f64 = 24.0;

const CSS: &str = r#"
.canvas { background-color: color-mix(in srgb, var(--view-bg-color) 70%, black); }
.canvas picture { margin: CANVAS_MARGINpx; }
.canvas.bg-black { background-color: #000; }
.canvas.bg-white { background-color: #fff; }
.sidebar-pane { background-color: var(--sidebar-bg-color); }
.section { padding: 4px 8px 10px 8px; }
.section.collapsed { padding-bottom: 4px; }
.section-header { padding: 6px 6px; margin-bottom: 2px; border-radius: 8px; }
.slider-row { margin: 2px 4px 0 4px; }
.slider-value { opacity: 0.8; }
.slider-reset { min-width: 24px; min-height: 24px; padding: 0; }
scale.track-temp trough { background-image: linear-gradient(to right, #4f86e8, #e8e8e8, #f2b84b); }
scale.track-tint trough { background-image: linear-gradient(to right, #4cbf5a, #e8e8e8, #d85bd0); }
scale.track-temp trough highlight, scale.track-tint trough highlight { background: none; }
.scope { background-color: rgba(0,0,0,0.55); border-radius: 8px; }
.scope-grip { padding: 3px 0; }
.scope-grip > box { min-height: 4px; min-width: 36px; border-radius: 2px; background-color: alpha(currentColor, 0.2); }
.scope-grip:hover > box { background-color: alpha(currentColor, 0.45); }
.band { min-width: 22px; min-height: 22px; padding: 0; border-radius: 99px; }
.band:checked { box-shadow: 0 0 0 2px var(--accent-color); }
.bypassed { opacity: 0.45; }
@keyframes flash { from { background-color: color-mix(in srgb, var(--accent-bg-color) 45%, transparent); } }
row.flash { animation: flash 900ms ease-out; }
flowboxchild { padding: 0; border-radius: 12px; }
flowboxchild:selected { background: none; }
.thumb-card { padding: 6px; border-radius: 12px; }
.thumb-card picture { border-radius: 8px; }
flowboxchild:selected .thumb-card { background-color: color-mix(in srgb, var(--accent-bg-color) 35%, transparent); }
.lut-badge { margin: 5px; padding: 1px 6px; border-radius: 6px; font-size: 0.68em; font-weight: 800;
  letter-spacing: 0.04em; color: rgba(255,255,255,0.92); background-color: rgba(0,0,0,0.45); }
.lab-summary { padding: 8px 10px; border-radius: 8px; background-color: color-mix(in srgb, var(--accent-bg-color) 14%, transparent); }
.osd-info { padding: 4px 10px; border-radius: 99px; font-size: 0.9em; }
scale.track-ch0 trough { background-image: linear-gradient(to right, #1b1b1b, #e5534b); }
scale.track-ch1 trough { background-image: linear-gradient(to right, #1b1b1b, #57c46a); }
scale.track-ch2 trough { background-image: linear-gradient(to right, #1b1b1b, #5b8def); }
scale.track-film-sat trough { background-image: linear-gradient(to right, #8a8580, #b34a3c, #5a1f2a); }
scale.track-film-density trough { background-image: linear-gradient(to right, #d9774f, #7a2c22); }
scale.track-ch0 trough highlight, scale.track-ch1 trough highlight, scale.track-ch2 trough highlight,
scale.track-film-sat trough highlight, scale.track-film-density trough highlight { background: none; }
.reference { border-radius: 8px; }
.canvas-tools { border-radius: 10px; }
.clip-swatch { min-width: 10px; min-height: 10px; border-radius: 3px; }
.clip-hi { background-color: #ff2929; }
.clip-lo { background-color: #2e6bff; }
.clip-gamut { background-color: #ffdb1a; }
.filmstrip { background-color: color-mix(in srgb, var(--view-bg-color) 55%, black);
  border-top: 1px solid color-mix(in srgb, var(--borders-color) 60%, transparent); }
.roll-card, .grid-card { padding: 4px; border-radius: 10px; }
.roll-card picture, .grid-card picture { border-radius: 6px; }
.roll-card.active, .grid-card.active { box-shadow: inset 0 0 0 2px var(--accent-color);
  background-color: color-mix(in srgb, var(--accent-bg-color) 18%, transparent); }
.roll-remove { min-width: 22px; min-height: 22px; padding: 0; opacity: 0; transition: opacity 150ms ease; }
.roll-item:hover .roll-remove, .roll-remove:focus-visible { opacity: 1; }
@media (prefers-contrast: more) {
  .roll-card.active, .grid-card.active { box-shadow: inset 0 0 0 3px currentColor; }
  .roll-remove { opacity: 1; }
}
"#;

/// Selective-colour band names; their hues are `grade::BAND_HUES`.
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
/// CSS class prefixes of the band's hue / saturation / luminance tracks.
const BAND_TRACKS: [&str; 3] = ["hue", "sat", "lum"];

/// Swatches and slider tracks computed in OKLCh from the shader's own band
/// hues, so each track shows exactly what moving it does to that band.
fn band_css() -> String {
    let hex = |l: f32, c: f32, h: f32| {
        let [r, g, b] = oklch_to_srgb(l, c, h).map(|v| (v * 255.0).round() as u8);
        format!("#{r:02x}{g:02x}{b:02x}")
    };
    let mut css = String::new();
    for (i, &h) in BAND_HUES.iter().enumerate() {
        let hue: Vec<String> =
            [-1.0, -0.5, 0.0, 0.5, 1.0].iter().map(|t| hex(0.72, 0.13, h + t * BAND_HUE_RANGE)).collect();
        css += &format!(".band-{i} {{ background: {}; }}\n", hex(0.68, 0.15, h));
        css +=
            &format!("scale.hue-{i} trough {{ background-image: linear-gradient(to right, {}); }}\n", hue.join(", "));
        css += &format!(
            "scale.sat-{i} trough {{ background-image: linear-gradient(to right, {}, {}); }}\n",
            hex(0.72, 0.0, h),
            hex(0.72, 0.2, h)
        );
        css += &format!(
            "scale.lum-{i} trough {{ background-image: linear-gradient(to right, {}, {}); }}\n",
            hex(0.38, 0.1, h),
            hex(0.92, 0.1, h)
        );
    }
    css
}

/// Channel mixer controls: one page of three sliders per output channel,
/// and one for black & white, all views of `Params::mixer`.
struct MixerUi {
    rows: adw::ToggleGroup,
    pages: gtk::Stack,
    /// Output R, G, B rows, then the monochrome filter: (slider, `mixer_value` index, base).
    sliders: Vec<(Slider, usize, f64)>,
    mono: adw::SwitchRow,
    keep: adw::SwitchRow,
}

/// Reference match: empty state, or the reference with its strength.
struct MatchUi {
    stack: gtk::Stack,
    pic: gtk::Picture,
    name: gtk::Label,
    amount: Slider,
    /// "Camera JPEG": RAWs only.
    camera: gtk::Button,
}

struct Sidebar {
    /// The pinned scopes (when pinned) over `scroll`.
    pane: gtk::Box,
    scroll: gtk::ScrolledWindow,
    sliders: Vec<(Slider, Field)>,
    wheels: Vec<(ColorWheel, Slider)>,
    hue: [Slider; 3],
    curves: CurveEditor,
    hue_curves: CurveEditor,
    mixer: MixerUi,
    matching: MatchUi,
    scopes: [gtk::Picture; 3],
    skin: gtk::DrawingArea,
    skin_btn: gtk::ToggleButton,
    looks: gtk::ListBox,
    /// Chain / Mix of the looks, shown with two or more.
    look_blend: gtk::Box,
    look_blend_toggle: adw::ToggleGroup,
    /// The LUT browser, revealed under the look stack.
    browse: gtk::Revealer,
    browse_btn: gtk::ToggleButton,
    develop: DevelopUi,
    /// The Tone "As in camera" button (RAWs only).
    camera_btn: gtk::Button,
}

/// The Develop group (RAWs only).
struct DevelopUi {
    /// Set while `sync_develop` moves the controls: their signals are not edits.
    syncing: Rc<Cell<bool>>,
    root: gtk::Box,
    preset: gtk::DropDown,
    kelvin: Slider,
    tint: Slider,
    picker: gtk::ToggleButton,
    exposure: Slider,
}

/// Index of "Custom" in the white-balance preset dropdown (after As shot and the presets).
const WB_CUSTOM: u32 = WB_PRESETS.len() as u32 + 1;

fn build_develop(sender: &ComponentSender<App>) -> DevelopUi {
    let root = vbox(12);
    root.append(&group_heading(&gettext("Develop · not in LUT")));

    let wb = vbox(4);
    let names: Vec<String> = std::iter::once(gettext("As shot"))
        // Same order as WB_PRESETS (literal calls, so xgettext finds them).
        .chain([
            gettext("Daylight"),
            gettext("Cloudy"),
            gettext("Shade"),
            gettext("Tungsten"),
            gettext("Fluorescent"),
            gettext("Flash"),
        ])
        .chain([gettext("Custom")])
        .collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let preset = gtk::DropDown::from_strings(&names);
    preset.set_hexpand(true);
    preset.update_property(&[gtk::accessible::Property::Label(&gettext("White balance preset"))]);
    let syncing = Rc::new(Cell::new(false));
    // Only the user's own moves become edits, never `sync_develop` setting the controls.
    let edit = |f: fn(f64) -> DevelopEdit| {
        let (snd, syncing) = (sender.clone(), syncing.clone());
        move |v: f64| {
            if !syncing.get() {
                snd.input(Msg::Develop(f(v)));
            }
        }
    };
    let on_preset = edit(|v| DevelopEdit::Preset(v as u32));
    preset.connect_selected_notify(move |d| on_preset(d.selected() as f64));
    let picker = gtk::ToggleButton::builder()
        .icon_name("color-select-symbolic")
        .tooltip_text(gettext("Pick a neutral grey on the photo (Escape cancels)"))
        .css_classes(["flat"])
        .build();
    picker.update_property(&[gtk::accessible::Property::Label(&gettext("White balance picker"))]);
    let snd = sender.clone();
    picker.connect_toggled(move |b| snd.input(Msg::Develop(DevelopEdit::Pick(b.is_active()))));
    let row = gtk::Box::builder().spacing(6).css_classes(["slider-row"]).build();
    row.append(&preset);
    row.append(&picker);
    wb.append(&row);
    let kelvin =
        Slider::new(&gettext("Temperature"), KELVIN[0] as f64, KELVIN[1] as f64, 5500.0, 0, Some("track-temp"));
    kelvin.connect(edit(|v| DevelopEdit::Kelvin(v as f32)));
    let tint = Slider::new(&gettext("Tint"), -100.0, 100.0, 0.0, 0, Some("track-tint"));
    tint.connect(edit(|v| DevelopEdit::Tint(v as f32)));
    wb.append(&kelvin.root);
    wb.append(&tint.root);
    let as_shot =
        camera_button(&gettext("As in camera: white balance as shot"), || Msg::Develop(DevelopEdit::AsShot), sender);
    root.append(&section_with(&gettext("White balance"), &wb, &[as_shot.upcast()], true));

    let exp = vbox(4);
    let exposure = Slider::new(&gettext("Exposure"), -3.0, 3.0, 0.0, 2, None);
    exposure.connect(edit(|v| DevelopEdit::Exposure(v as f32)));
    exp.append(&exposure.root);
    let cam_ev = camera_button(
        &gettext("As in camera: the exposure the camera asks for"),
        || Msg::Develop(DevelopEdit::CameraExposure),
        sender,
    );
    root.append(&section_with(&gettext("Exposure"), &exp, &[cam_ev.upcast()], true));
    root.set_visible(false);
    DevelopUi { syncing, root, preset, kelvin, tint, picker, exposure }
}

/// Curve editor with a channel selector and a reset button.
fn curve_panel(editor: &CurveEditor, names: &[(&str, &str)], hint: &str, sender: &ComponentSender<App>) -> gtk::Box {
    let sel = toggles(names);
    let ce = editor.clone();
    sel.connect_active_name_notify(move |g| {
        if let Some(ch) = g.active_name().and_then(|n| n.parse().ok()) {
            ce.set_active(ch);
        }
    });
    let reset =
        gtk::Button::builder().label(gettext("Reset curve")).css_classes(["flat"]).halign(gtk::Align::End).build();
    let (ce, snd) = (editor.clone(), sender.clone());
    reset.connect_clicked(move |_| {
        let (ch, c) = ce.reset_active();
        snd.input(Msg::Curve(ch, c));
    });
    let bx = vbox(8);
    bx.append(&sel);
    bx.append(&editor.area);
    bx.append(&caption(hint));
    bx.append(&reset);
    bx
}

fn build_mixer(sender: &ComponentSender<App>) -> (gtk::Box, MixerUi) {
    let names = [gettext("Red"), gettext("Green"), gettext("Blue")];
    let rows = toggles(&[("0", names[0].as_str()), ("1", names[1].as_str()), ("2", names[2].as_str())]);
    let pages = gtk::Stack::builder().transition_type(gtk::StackTransitionType::Crossfade).vhomogeneous(false).build();
    let mut sliders = Vec::new();
    // Output rows: 100% of their own channel by default.
    for row in 0..3 {
        let page = vbox(4);
        for (col, name) in names.iter().enumerate() {
            let base = if row == col { 100.0 } else { 0.0 };
            let s = Slider::new(name, -200.0, 200.0, base, 0, Some(&format!("track-ch{col}")));
            let (snd, i) = (sender.clone(), row * 3 + col);
            s.connect(move |v| snd.input(Msg::Mixer(i, v - base)));
            page.append(&s.root);
            sliders.push((s, i, base));
        }
        pages.add_named(&page, Some(&row.to_string()));
    }
    // Black & white: luminance weights by default, plus a colour filter.
    let mono_page = vbox(4);
    for (col, w) in LUMA.into_iter().enumerate() {
        let base = (w as f64 * 1000.0).round() / 10.0;
        let s = Slider::new(&names[col], -200.0, 200.0, base, 0, Some(&format!("track-ch{col}")));
        let snd = sender.clone();
        s.connect(move |v| snd.input(Msg::Mixer(9 + col, v - base)));
        mono_page.append(&s.root);
        sliders.push((s, 9 + col, base));
    }
    mono_page.append(&caption(&gettext(
        "Like a colour filter on black & white film: more red brightens skin and darkens skies.",
    )));
    pages.add_named(&mono_page, Some("mono"));
    let p = pages.clone();
    rows.connect_active_name_notify(move |g| {
        if let Some(n) = g.active_name() {
            p.set_visible_child_name(&n);
        }
    });

    let mono = adw::SwitchRow::builder().title(gettext("Black & white")).use_markup(false).build();
    let keep = adw::SwitchRow::builder()
        .title(gettext("Preserve luminance"))
        .subtitle(gettext("The mix changes colour, not brightness"))
        .active(true)
        .build();
    let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();
    list.append(&mono);
    list.append(&keep);
    let snd = sender.clone();
    mono.connect_active_notify(move |m| snd.input(Msg::MixerMono(m.is_active())));
    let snd = sender.clone();
    keep.connect_active_notify(move |k| snd.input(Msg::MixerFree(!k.is_active())));

    let bx = vbox(8);
    bx.append(&list);
    bx.append(&rows);
    bx.append(&pages);
    bx.append(&caption(&gettext(
        "Each output channel is a mix of the scene's red, green and blue; 100% of its own channel is no change.",
    )));
    (bx, MixerUi { rows, pages, sliders, mono, keep })
}

fn build_match(sender: &ComponentSender<App>) -> MatchUi {
    let choose = |label: String| {
        let b = gtk::Button::builder().label(label).css_classes(["pill"]).halign(gtk::Align::Center).build();
        let snd = sender.clone();
        b.connect_clicked(move |_| snd.input(Msg::MatchChoose));
        b
    };
    let empty = vbox(10);
    empty.append(&caption(&gettext(
        "Make this photo look like a reference: a film still, a photo you love. Tones and colours are matched; refine the rest with the other tools.",
    )));
    empty.append(&choose(gettext("Choose Reference…")));
    let camera = gtk::Button::builder()
        .label(gettext("Camera JPEG"))
        .tooltip_text(gettext("As in camera: match the JPEG the camera embedded in the RAW"))
        .css_classes(["flat"])
        .halign(gtk::Align::Center)
        .build();
    let snd = sender.clone();
    camera.connect_clicked(move |_| snd.input(Msg::AsInCamera(CameraPart::Match)));
    empty.append(&camera);

    let busy = adw::Spinner::builder().height_request(32).margin_top(12).margin_bottom(12).build();

    let pic = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Cover)
        .height_request(110)
        .css_classes(["reference"])
        .build();
    pic.update_property(&[gtk::accessible::Property::Label(&gettext("Reference photo"))]);
    let name =
        gtk::Label::builder().ellipsize(gtk::pango::EllipsizeMode::Middle).css_classes(["caption"]).xalign(0.0).build();
    let amount = Slider::new(&gettext("Strength"), 0.0, 100.0, 100.0, 0, None);
    let snd = sender.clone();
    amount.connect(move |v| snd.input(Msg::Set(|p| &mut p.match_amount, v)));
    let remove = gtk::Button::builder().label(gettext("Remove")).css_classes(["flat"]).build();
    let snd = sender.clone();
    remove.connect_clicked(move |_| snd.input(Msg::MatchClear));
    let actions = gtk::Box::builder().spacing(6).halign(gtk::Align::End).build();
    let change = choose(gettext("Change…"));
    change.remove_css_class("pill");
    change.add_css_class("flat");
    actions.append(&change);
    actions.append(&remove);
    let set = vbox(6);
    set.append(&pic);
    set.append(&name);
    set.append(&amount.root);
    set.append(&actions);

    let stack = gtk::Stack::builder().transition_type(gtk::StackTransitionType::Crossfade).vhomogeneous(false).build();
    stack.add_named(&empty, Some("empty"));
    stack.add_named(&busy, Some("busy"));
    stack.add_named(&set, Some("set"));
    MatchUi { stack, pic, name, amount, camera }
}

/// Height range of the scopes, in logical pixels.
const SCOPES_MIN: i32 = 50;
const SCOPES_MAX: i32 = 320;

fn build_sidebar(sender: &ComponentSender<App>, crop: &gtk::Box) -> Sidebar {
    let panel = vbox(12);
    panel.set_margin_top(12);
    panel.set_margin_bottom(18);
    panel.set_margin_start(12);
    panel.set_margin_end(12);

    // Scopes
    let height = settings::value(settings::SCOPES_HEIGHT).and_then(|v| v.parse().ok()).unwrap_or(100);
    let scope_pics: [gtk::Picture; 3] = std::array::from_fn(|_| {
        gtk::Picture::builder().content_fit(gtk::ContentFit::Fill).css_classes(["scope"]).build()
    });
    scope_pics[2].set_content_fit(gtk::ContentFit::Contain);
    let skin = widgets::skin_line();
    skin.set_visible(false);
    let vec_layer = gtk::Overlay::builder().child(&scope_pics[2]).build();
    vec_layer.add_overlay(&skin);
    let scope_stack = gtk::Stack::builder().transition_type(gtk::StackTransitionType::Crossfade).build();
    scope_stack.add_named(&scope_pics[0], Some("hist"));
    scope_stack.add_named(&scope_pics[1], Some("wave"));
    scope_stack.add_named(&vec_layer, Some("vec"));
    let scope_sel = toggles(&[
        ("hist", gettext("Histogram").as_str()),
        ("wave", gettext("Waveform").as_str()),
        ("vec", gettext("Vectorscope").as_str()),
    ]);
    scope_sel.set_hexpand(true);
    let skin_btn = gtk::ToggleButton::builder()
        .icon_name("avatar-default-symbolic")
        .tooltip_text(gettext("Skin tone line (K)"))
        .css_classes(["flat", "circular"])
        .valign(gtk::Align::Center)
        .visible(false)
        .build();
    skin_btn.update_property(&[gtk::accessible::Property::Label(&gettext("Skin tone line"))]);
    let snd = sender.clone();
    skin_btn.connect_toggled(move |b| snd.input(Msg::SkinLine(b.is_active())));
    let (st, sb) = (scope_stack.clone(), skin_btn.clone());
    scope_sel.connect_active_name_notify(move |g| {
        if let Some(n) = g.active_name() {
            st.set_visible_child_name(&n);
            sb.set_visible(n == "vec");
        }
    });
    let scope_head = gtk::Box::builder().spacing(6).build();
    scope_head.append(&scope_sel);
    scope_head.append(&skin_btn);
    let scopes_box = vbox(8);
    scopes_box.append(&scope_head);
    // A picture asks for its natural height and will not shrink below it: the
    // scopes overlay a box of the chosen height, which alone sets the size.
    let sizer = gtk::Box::builder().height_request(height).build();
    let frame = gtk::Overlay::builder().child(&sizer).build();
    frame.add_overlay(&scope_stack);
    scopes_box.append(&frame);
    // Drag the grip under the scopes to resize them.
    let grip = gtk::Box::builder().css_classes(["scope-grip"]).tooltip_text(gettext("Drag to resize")).build();
    grip.append(&gtk::Box::builder().hexpand(true).halign(gtk::Align::Center).build());
    grip.set_cursor_from_name(Some("ns-resize"));
    // Tracked against the frame, which stays put: the grip moves with the
    // height, so its own drag offsets would feed back and flicker.
    let resize = gtk::GestureDrag::new();
    let grab = std::rc::Rc::new(std::cell::Cell::new(0.0));
    let in_frame = {
        let (g, f) = (grip.clone(), frame.clone());
        move |x: f64, y: f64| g.compute_point(&f, &gtk::graphene::Point::new(x as f32, y as f32)).map(|p| p.y())
    };
    let (sz, gr, at) = (sizer.clone(), grab.clone(), in_frame.clone());
    resize.connect_drag_begin(move |_, x, y| {
        if let Some(fy) = at(x, y) {
            gr.set(fy - sz.height_request() as f32)
        }
    });
    let sz = sizer.clone();
    resize.connect_drag_update(move |g, dx, dy| {
        let Some((x, y)) = g.start_point() else { return };
        if let Some(fy) = in_frame(x + dx, y + dy) {
            sz.set_height_request(((fy - grab.get()) as i32).clamp(SCOPES_MIN, SCOPES_MAX));
        }
    });
    resize.connect_drag_end(move |_, _, _| {
        let _ = settings::set_value(settings::SCOPES_HEIGHT, &sizer.height_request().to_string());
    });
    grip.add_controller(resize);
    scopes_box.append(&grip);
    let pin = gtk::ToggleButton::builder()
        .icon_name("view-pin-symbolic")
        .tooltip_text(gettext("Keep scopes in view while scrolling"))
        .valign(gtk::Align::Center)
        .css_classes(["flat", "circular"])
        .build();
    pin.update_property(&[gtk::accessible::Property::Label(&gettext("Pin scopes"))]);
    let scopes = section_with(&gettext("Scopes"), &scopes_box, &[pin.clone().upcast()], true);
    panel.append(&scopes);
    let develop = build_develop(sender);
    panel.append(&develop.root);
    panel.append(&group_heading(&gettext("Grade · in LUT")));

    // Input profile: the signal the LUT will receive in camera.
    let profile_box = vbox(6);
    let profile_sel = lab_page::profile_dropdown(Profile::Standard);
    profile_sel.update_property(&[gtk::accessible::Property::Label(&gettext("Input profile"))]);
    let hint = caption(&profile_hint(Profile::Standard));
    let (snd, h) = (sender.clone(), hint.clone());
    profile_sel.connect_selected_notify(move |d| {
        let profile = Profile::ALL[d.selected() as usize];
        h.set_label(&profile_hint(profile));
        snd.input(Msg::Profile(profile));
    });
    profile_box.append(&profile_sel);
    profile_box.append(&hint);
    panel.append(&section(&gettext("Input profile"), &profile_box));

    // Reference match comes first: the rest refines it.
    let matching = build_match(sender);
    panel.append(&bypassable(&gettext("Reference match"), &matching.stack, Some(Stage::Match), false, sender));

    let mut sliders = Vec::new();
    let mut add = |bx: &gtk::Box, label: &str, min: f64, max: f64, digits: u32, track: Option<&str>, field: Field| {
        let s = Slider::new(label, min, max, 0.0, digits, track);
        let snd = sender.clone();
        s.connect(move |v| snd.input(Msg::Set(field, v)));
        bx.append(&s.root);
        sliders.push((s, field));
    };

    let wb = vbox(4);
    add(&wb, &gettext("Temperature"), -100.0, 100.0, 0, Some("track-temp"), |p| &mut p.temperature);
    add(&wb, &gettext("Tint"), -100.0, 100.0, 0, Some("track-tint"), |p| &mut p.tint);
    panel.append(&bypassable(&gettext("Warmth"), &wb, Some(Stage::WhiteBalance), true, sender));

    let tone = vbox(4);
    add(&tone, &gettext("Exposure"), -3.0, 3.0, 2, None, |p| &mut p.exposure);
    add(&tone, &gettext("Contrast"), -100.0, 100.0, 0, None, |p| &mut p.contrast);
    add(&tone, &gettext("Highlights"), -100.0, 100.0, 0, None, |p| &mut p.highlights);
    add(&tone, &gettext("Shadows"), -100.0, 100.0, 0, None, |p| &mut p.shadows);
    add(&tone, &gettext("Whites"), -100.0, 100.0, 0, None, |p| &mut p.whites);
    add(&tone, &gettext("Blacks"), -100.0, 100.0, 0, None, |p| &mut p.blacks);
    let tone_cam =
        camera_button(&gettext("As in camera: the camera's tones"), || Msg::AsInCamera(CameraPart::Tone), sender);
    panel.append(&bypassable_with(
        &gettext("Tone"),
        &tone,
        Some(Stage::Tone),
        true,
        &[tone_cam.clone().upcast()],
        sender,
    ));

    let (mixer_box, mixer) = build_mixer(sender);
    panel.append(&bypassable(&gettext("Channel mixer"), &mixer_box, Some(Stage::Mixer), false, sender));

    let color = vbox(4);
    add(&color, &gettext("Saturation"), -100.0, 100.0, 0, None, |p| &mut p.saturation);
    add(&color, &gettext("Vibrance"), -100.0, 100.0, 0, None, |p| &mut p.vibrance);
    add(&color, &gettext("Shadow saturation"), -100.0, 100.0, 0, None, |p| &mut p.sat_shadows);
    add(&color, &gettext("Highlight saturation"), -100.0, 100.0, 0, None, |p| &mut p.sat_highlights);
    panel.append(&bypassable(&gettext("Color"), &color, Some(Stage::Color), true, sender));

    let film = vbox(4);
    add(&film, &gettext("Subtractive saturation"), -100.0, 100.0, 0, Some("track-film-sat"), |p| &mut p.film_sat);
    add(&film, &gettext("Color density"), 0.0, 100.0, 0, Some("track-film-density"), |p| &mut p.film_density);
    film.append(&caption(&gettext(
        "Film makes colour by adding dye, so richer colours are also deeper. Subtractive saturation saturates the way dye does; density deepens saturated colours. Greys are untouched.",
    )));
    panel.append(&bypassable(&gettext("Film color"), &film, Some(Stage::Film), true, sender));

    // HSL mixer (OKLCh bands)
    let hsl = vbox(6);
    let names = band_names();
    let band_name = gtk::Label::builder().label(names[0].as_str()).css_classes(["caption-heading"]).build();
    let dots = gtk::Box::builder().spacing(6).halign(gtk::Align::Center).build();
    let mut first: Option<gtk::ToggleButton> = None;
    for (i, name) in names.iter().enumerate() {
        let b = gtk::ToggleButton::builder()
            .tooltip_text(name.as_str())
            .css_classes(["band", &format!("band-{i}")])
            .build();
        b.update_property(&[gtk::accessible::Property::Label(name)]);
        match &first {
            None => {
                b.set_active(true);
                first = Some(b.clone());
            }
            Some(f) => b.set_group(Some(f)),
        }
        let (snd, bn, name) = (sender.clone(), band_name.clone(), name.clone());
        b.connect_toggled(move |b| {
            if b.is_active() {
                bn.set_label(&name);
                snd.input(Msg::Band(i));
            }
        });
        dots.append(&b);
    }
    hsl.append(&dots);
    hsl.append(&band_name);
    let hue: [Slider; 3] = std::array::from_fn(|k| {
        let label = [gettext("Hue"), gettext("Saturation"), gettext("Luminance")][k].clone();
        let s = Slider::new(&label, -100.0, 100.0, 0.0, 0, Some(&format!("{}-0", BAND_TRACKS[k])));
        let snd = sender.clone();
        s.connect(move |v| snd.input(Msg::Hue(k, v)));
        hsl.append(&s.root);
        s
    });
    panel.append(&bypassable(&gettext("Selective color"), &hsl, Some(Stage::Selective), true, sender));

    let snd = sender.clone();
    let hue_curves = CurveEditor::new(HUE_HUE..LUM_SAT + 1, move |ch, c| snd.input(Msg::Curve(ch, c)));
    let hue_box = curve_panel(
        &hue_curves,
        &[
            (&HUE_HUE.to_string(), gettext("Hue vs Hue").as_str()),
            (&HUE_SAT.to_string(), gettext("Hue vs Sat").as_str()),
            (&LUM_SAT.to_string(), gettext("Lum vs Sat").as_str()),
        ],
        &gettext(
            "The middle line is no change. Pull a hue up or down to shift it, saturate or desaturate it; Lum vs Sat works by brightness.",
        ),
        sender,
    );
    panel.append(&bypassable(&gettext("Hue curves"), &hue_box, Some(Stage::HueCurves), false, sender));

    // Primaries: lift / gamma / gain / offset wheels with a master slider each.
    let wheels_row = gtk::Grid::builder().column_spacing(10).row_spacing(10).column_homogeneous(true).build();
    let mut wheels = Vec::new();
    for (i, name) in ["Lift", "Gamma", "Gain", "Offset"].into_iter().enumerate() {
        let col = vbox(4);
        col.append(&gtk::Label::builder().label(name).css_classes(["caption-heading"]).build());
        let snd = sender.clone();
        let wheel = ColorWheel::new(name, move |x, y| snd.input(Msg::Wheel(i, x, y)));
        col.append(&wheel.area);
        let luma = Slider::new("Luma", -100.0, 100.0, 0.0, 0, None);
        let snd = sender.clone();
        luma.connect(move |v| snd.input(Msg::WheelLuma(i, v)));
        col.append(&luma.root);
        wheels_row.attach(&col, (i % 2) as i32, (i / 2) as i32, 1, 1);
        wheels.push((wheel, luma));
    }
    panel.append(&bypassable(&gettext("Primary color wheels"), &wheels_row, Some(Stage::Wheels), true, sender));

    // Curves
    let snd = sender.clone();
    let curves = CurveEditor::new(MASTER..HUE_HUE, move |ch, c| snd.input(Msg::Curve(ch, c)));
    let curve_box = curve_panel(
        &curves,
        &[
            ("0", "Luma"),
            ("1", gettext("Red").as_str()),
            ("2", gettext("Green").as_str()),
            ("3", gettext("Blue").as_str()),
        ],
        &gettext("Click to add a point, double-click or right-click to remove it"),
        sender,
    );
    panel.append(&bypassable(&gettext("Curves"), &curve_box, Some(Stage::Curves), true, sender));

    // LUTs after the grade
    let look_box = vbox(8);
    let looks = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();
    let empty = caption(&gettext("No LUTs: the grade is exported as is."));
    empty.set_margin_top(12);
    empty.set_margin_bottom(12);
    empty.set_margin_start(12);
    empty.set_margin_end(12);
    looks.set_placeholder(Some(&empty));
    // How looks combine: only shown with two or more.
    let (blend, hint) = lab_page::blend_toggle(sender, Msg::LookBlend);
    let look_blend_toggle = blend.clone();
    let look_blend = vbox(6);
    look_blend.set_visible(false);
    look_blend.append(&blend);
    look_blend.append(&hint);
    look_box.append(&look_blend);
    look_box.append(&looks);
    let browse_btn = gtk::ToggleButton::builder()
        .child(&adw::ButtonContent::builder().icon_name("view-grid-symbolic").label(gettext("Browse LUTs")).build())
        .tooltip_text(gettext("Browse the LUT library on this photo (B)"))
        .css_classes(["pill"])
        .build();
    let snd = sender.clone();
    browse_btn.connect_toggled(move |b| snd.input(Msg::Browse(b.is_active())));
    let from_file = gtk::Button::builder()
        .icon_name("document-open-symbolic")
        .tooltip_text(gettext("Add a LUT from a file"))
        .css_classes(["flat", "circular"])
        .action_name("win.look-add")
        .action_target(&"file".to_variant())
        .build();
    from_file.update_property(&[gtk::accessible::Property::Label(&gettext("Add a LUT from a file"))]);
    let buttons = gtk::Box::builder().spacing(6).build();
    buttons.append(&browse_btn);
    buttons.append(&from_file);
    look_box.append(&buttons);
    let browse = gtk::Revealer::builder().transition_type(gtk::RevealerTransitionType::SlideDown).build();
    look_box.append(&browse);
    look_box.append(&caption(&gettext("Applied after the grade, top to bottom, and included in the exported LUT.")));
    panel.append(&bypassable(&gettext("LUTs"), &look_box, None, true, sender));
    panel.append(&group_heading(&gettext("Finish · not in LUT")));
    panel.append(crop);

    let reset = gtk::Button::builder()
        .label(gettext("Reset all"))
        .css_classes(["pill"])
        .halign(gtk::Align::Center)
        .margin_top(6)
        .build();
    let snd = sender.clone();
    reset.connect_clicked(move |_| snd.input(Msg::ResetAll));
    panel.append(&reset);

    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&panel)
        .vexpand(true)
        .css_classes(["sidebar-pane"])
        .build();
    let pane = vbox(0);
    pane.add_css_class("sidebar-pane");
    pane.append(&scroll);
    // Pinned: the scopes card leaves the scrolled panel for the top of the pane.
    let (p, sc) = (pane.clone(), panel.clone());
    pin.connect_toggled(move |b| {
        let on = b.is_active();
        let m = if on { 12 } else { 0 };
        scopes.set_margin_top(m);
        scopes.set_margin_start(m);
        scopes.set_margin_end(m);
        scopes.set_margin_bottom(m / 2);
        scopes.unparent();
        if on {
            p.prepend(&scopes)
        } else {
            sc.prepend(&scopes)
        }
        let _ = settings::set_value(settings::SCOPES_PINNED, if on { "1" } else { "0" });
    });
    pin.set_active(settings::value(settings::SCOPES_PINNED).as_deref() != Some("0"));
    Sidebar {
        pane,
        scroll,
        sliders,
        wheels,
        hue,
        curves,
        hue_curves,
        mixer,
        matching,
        scopes: scope_pics,
        skin,
        skin_btn,
        looks,
        look_blend,
        browse,
        browse_btn,
        develop,
        camera_btn: tone_cam,
        look_blend_toggle,
    }
}

fn empty_state(
    sender: &ComponentSender<App>,
    icon: &str,
    title: &str,
    desc: &str,
    button: &str,
    msg: fn() -> Msg,
) -> adw::StatusPage {
    let b = gtk::Button::builder()
        .label(button)
        .css_classes(["pill", "suggested-action"])
        .halign(gtk::Align::Center)
        .build();
    let s = sender.clone();
    b.connect_clicked(move |_| s.input(msg()));
    adw::StatusPage::builder().icon_name(icon).title(title).description(desc).child(&b).build()
}

impl SimpleComponent for App {
    type Init = ();
    type Input = Msg;
    type Output = ();
    type Root = adw::ApplicationWindow;
    type Widgets = ();

    fn init_root() -> Self::Root {
        adw::ApplicationWindow::builder()
            .default_width(1440)
            .default_height(900)
            .width_request(360)
            .height_request(300)
            .title("RawValley")
            .build()
    }

    fn init(_: Self::Init, window: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        // Neutral dark surround is the colourist standard.
        adw::StyleManager::default().set_color_scheme(adw::ColorScheme::PreferDark);

        let snd = sender.clone();
        let jobs = gpu::spawn(move |d| snd.input(Msg::Gpu(d)));

        // ---------------- grade page
        let grade_pic = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::Contain)
            .can_shrink(true)
            .hexpand(true)
            .vexpand(true)
            .build();
        grade_pic.update_property(&[gtk::accessible::Property::Label(&gettext("Grade preview"))]);
        add_split_drag(&grade_pic, &sender);
        let info = gtk::Label::builder()
            .css_classes(["osd", "osd-info"])
            .halign(gtk::Align::Start)
            .valign(gtk::Align::End)
            .margin_start(16)
            .margin_bottom(16)
            .build();
        let canvas = gtk::Overlay::builder().child(&grade_pic).css_classes(["canvas"]).build();
        canvas.add_overlay(&info);
        let zoom = ZoomUi::new(&canvas, &sender);
        let crop_ui = CropUi::new(&sender);
        canvas.add_overlay(&crop_ui.overlay);
        // Preview aids, top right: clipping stripes and the filmstrip grid.
        let clip_btn = gtk::ToggleButton::builder()
            .icon_name("dialog-warning-symbolic")
            .tooltip_text(gettext("Show clipping (C)"))
            .build();
        clip_btn.update_property(&[gtk::accessible::Property::Label(&gettext("Show clipping"))]);
        let snd = sender.clone();
        clip_btn.connect_toggled(move |b| snd.input(Msg::Overlay(b.is_active())));
        let grid_btn = gtk::ToggleButton::builder()
            .icon_name("view-grid-symbolic")
            .tooltip_text(gettext("All photos in the filmstrip (G)"))
            .sensitive(false)
            .build();
        grid_btn.update_property(&[gtk::accessible::Property::Label(&gettext("Grid of all photos"))]);
        let snd = sender.clone();
        grid_btn.connect_toggled(move |b| snd.input(Msg::Grid(b.is_active())));
        let tools = gtk::Box::builder()
            .css_classes(["linked", "osd", "canvas-tools"])
            .halign(gtk::Align::End)
            .valign(gtk::Align::Start)
            .margin_top(12)
            .margin_end(12)
            .build();
        tools.append(&clip_btn);
        tools.append(&grid_btn);
        // What the stripes mean, shown while they are.
        let legend_box = gtk::Box::builder().spacing(12).css_classes(["osd", "osd-info"]).build();
        for (class, label) in [
            ("clip-hi", gettext("Clipped highlights")),
            ("clip-lo", gettext("Crushed blacks")),
            ("clip-gamut", gettext("Out of gamut")),
        ] {
            let item = gtk::Box::builder().spacing(6).build();
            item.append(&gtk::Box::builder().css_classes(["clip-swatch", class]).valign(gtk::Align::Center).build());
            item.append(&gtk::Label::new(Some(&label)));
            legend_box.append(&item);
        }
        let legend = gtk::Revealer::builder()
            .child(&legend_box)
            .transition_type(gtk::RevealerTransitionType::Crossfade)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::End)
            .margin_bottom(16)
            .build();
        canvas.add_overlay(&legend);
        let filmstrip = Filmstrip::new(&sender);
        let canvas_stack =
            gtk::Stack::builder().transition_type(gtk::StackTransitionType::Crossfade).vexpand(true).build();
        canvas_stack.add_named(&canvas, Some("single"));
        canvas_stack.add_named(&filmstrip.grid_root, Some("grid"));
        let stage = gtk::Overlay::builder().child(&canvas_stack).build();
        stage.add_overlay(&tools);
        let image_page = vbox(0);
        image_page.append(&stage);
        image_page.append(&filmstrip.root);
        let loading = adw::Spinner::builder()
            .width_request(48)
            .height_request(48)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();
        let grade_stack = gtk::Stack::builder().transition_type(gtk::StackTransitionType::Crossfade).build();
        grade_stack.add_named(
            &empty_state(
                &sender,
                "image-x-generic-symbolic",
                &gettext("Open a photo"),
                &gettext("Drop JPEGs or camera RAWs here, or choose files. Open several to grade them together.\nThe grade becomes a .cube LUT for the Standard profile."),
                &gettext("Open Photo…"),
                || Msg::Open,
            ),
            Some("empty"),
        );
        grade_stack.add_named(&loading, Some("loading"));
        grade_stack.add_named(&image_page, Some("image"));

        let sidebar = build_sidebar(&sender, &crop_ui.section);
        let grade_split = adw::OverlaySplitView::builder()
            .sidebar_position(gtk::PackType::End)
            .min_sidebar_width(340.0)
            .max_sidebar_width(420.0)
            .sidebar_width_fraction(0.28)
            .content(&grade_stack)
            .sidebar(&sidebar.pane)
            .build();

        // ---------------- LUT browser
        let flow = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            // A click previews; Enter or a double-click adds.
            .activate_on_single_click(false)
            .min_children_per_line(2)
            .max_children_per_line(2)
            .homogeneous(true)
            .valign(gtk::Align::Start)
            .row_spacing(6)
            .column_spacing(6)
            .margin_start(8)
            .margin_end(8)
            .margin_bottom(12)
            .build();
        let snd = sender.clone();
        flow.connect_child_activated(move |_, c| snd.input(Msg::Commit(c.index() as usize)));
        let snd = sender.clone();
        flow.connect_selected_children_changed(move |f| {
            if let Some(c) = f.selected_children().first() {
                snd.input(Msg::SelectLut(c.index() as usize));
            }
        });
        let leave = gtk::EventControllerMotion::new();
        let snd = sender.clone();
        leave.connect_leave(move |_| snd.input(Msg::Hover(None)));
        flow.add_controller(leave);
        let folder_label = gtk::Label::builder()
            .label(gettext("No folder"))
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .xalign(0.0)
            .hexpand(true)
            .css_classes(["heading"])
            .build();
        let folder_btn = gtk::Button::builder()
            .icon_name("folder-open-symbolic")
            .tooltip_text(gettext("Choose LUT folder"))
            .css_classes(["flat"])
            .build();
        let snd = sender.clone();
        folder_btn.connect_clicked(move |_| snd.input(Msg::ChooseFolder));
        let folder_row =
            gtk::Box::builder().spacing(6).margin_start(12).margin_end(8).margin_top(8).margin_bottom(4).build();
        let to_lab = gtk::Button::builder()
            .icon_name("view-list-symbolic")
            .tooltip_text(gettext("Open the selected LUT in LUT Lab (to convert or combine it)"))
            .css_classes(["flat"])
            .build();
        let snd = sender.clone();
        to_lab.connect_clicked(move |_| snd.input(Msg::OpenInLab));
        folder_row.append(&folder_label);
        folder_row.append(&to_lab);
        let reload = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text(gettext("Reload LUTs (F5)"))
            .css_classes(["flat"])
            .build();
        let snd = sender.clone();
        reload.connect_clicked(move |_| snd.input(Msg::Reload));
        folder_row.append(&reload);
        folder_row.append(&folder_btn);
        let groups = gtk::DropDown::builder().visible(false).margin_start(8).margin_end(8).margin_bottom(4).build();
        groups.update_property(&[gtk::accessible::Property::Label(&gettext("Subfolder"))]);
        let snd = sender.clone();
        groups.connect_selected_notify(move |d| snd.input(Msg::Group(d.selected())));
        let search = gtk::SearchEntry::builder()
            .placeholder_text(gettext("Search LUTs"))
            .margin_start(8)
            .margin_end(8)
            .margin_bottom(6)
            .build();
        let snd = sender.clone();
        search.connect_search_changed(move |e| snd.input(Msg::Search(e.text().into())));
        let gallery_stack = gtk::Stack::new();
        gallery_stack.add_named(
            &adw::StatusPage::builder()
                .icon_name("view-grid-symbolic")
                .title(gettext("No LUTs"))
                .description(gettext("Choose a folder with .cube files"))
                .css_classes(["compact"])
                .build(),
            Some("empty"),
        );
        gallery_stack.add_named(
            &gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .child(&flow)
                .min_content_height(360)
                .max_content_height(520)
                .propagate_natural_height(true)
                .build(),
            Some("grid"),
        );
        let gallery = vbox(0);
        gallery.append(&folder_row);
        gallery.append(&groups);
        gallery.append(&search);
        gallery.append(&gallery_stack);

        sidebar.browse.set_child(Some(&gallery));
        // Once revealed: the sidebar scrolls to the browser and the grid takes the
        // keyboard, so arrows browse at once.
        let (f, btn, scroll) = (flow.clone(), sidebar.browse_btn.clone(), sidebar.scroll.clone());
        sidebar.browse.connect_child_revealed_notify(move |browse| {
            if !browse.is_child_revealed() {
                return;
            }
            if let Some(p) = scroll.child().and_then(|c| btn.compute_point(&c, &gtk::graphene::Point::zero())) {
                scroll.vadjustment().set_value(p.y() as f64 - 12.0);
            }
            // Ctrl+F put the focus in the search entry: leave it there.
            let inside = browse.root().and_then(|r| r.focus()).is_some_and(|f| f.is_ancestor(browse));
            if !inside {
                f.child_at_index(0).map_or_else(|| f.grab_focus(), |c| c.grab_focus());
            }
        });

        // ---------------- view stack + header
        let stack = adw::ViewStack::new();
        stack.add_titled_with_icon(&grade_split, Some("grade"), &gettext("Grade"), "color-select-symbolic");
        let lab = lab_page::build(&sender, &export_menu("lab-export", false));
        stack.add_titled_with_icon(&lab.root, Some("lab"), &gettext("LUT Lab"), "view-list-symbolic");
        let snd = sender.clone();
        stack.connect_visible_child_name_notify(move |s| {
            let page = match s.visible_child_name().as_deref() {
                Some("lab") => Page::Lab,
                _ => Page::Grade,
            };
            snd.input(Msg::Page(page));
        });

        let header = adw::HeaderBar::new();
        let switcher = adw::ViewSwitcher::builder().stack(&stack).policy(adw::ViewSwitcherPolicy::Wide).build();
        header.set_title_widget(Some(&switcher));

        let open = gtk::Button::builder()
            .icon_name("document-open-symbolic")
            .tooltip_text(gettext("Open Photo (Ctrl+O)"))
            .build();
        let snd = sender.clone();
        open.connect_clicked(move |_| snd.input(Msg::Open));
        header.pack_start(&open);
        let chart_btn = gtk::ToggleButton::builder()
            .icon_name("applications-science-symbolic")
            .tooltip_text(gettext("Test chart: shows what the LUT does to neutrals, hues and skin (T)"))
            .build();
        let snd = sender.clone();
        chart_btn.connect_toggled(move |b| snd.input(Msg::Chart(b.is_active())));
        header.pack_start(&chart_btn);
        let presets = presets_ui::build(&sender);
        header.pack_start(&presets.button);
        let camera_btn = gtk::Button::builder()
            .icon_name("camera-photo-symbolic")
            .tooltip_text(gettext("As in camera: white balance, exposure and tones from the camera"))
            .sensitive(false)
            .build();
        let snd = sender.clone();
        camera_btn.connect_clicked(move |_| snd.input(Msg::AsInCamera(CameraPart::All)));
        header.pack_start(&camera_btn);
        let clear_btn = gtk::Button::builder()
            .icon_name("edit-clear-all-symbolic")
            .tooltip_text(gettext("Reset all: clear every edit of this photo"))
            .build();
        clear_btn.update_property(&[gtk::accessible::Property::Label(&gettext("Reset all"))]);
        let snd = sender.clone();
        clear_btn.connect_clicked(move |_| snd.input(Msg::ResetAll));
        header.pack_start(&clear_btn);

        let menu = gio::Menu::new();
        let bg_menu = gio::Menu::new();
        bg_menu.append(Some(gettext("Default").as_str()), Some("win.background::default"));
        bg_menu.append(Some(gettext("Black").as_str()), Some("win.background::black"));
        bg_menu.append(Some(gettext("White").as_str()), Some("win.background::white"));
        menu.append_section(Some(gettext("Preview background").as_str()), &bg_menu);
        let section_menu = gio::Menu::new();
        section_menu.append(Some(gettext("Preferences").as_str()), Some("win.preferences"));
        section_menu.append(Some(gettext("Keyboard Shortcuts").as_str()), Some("win.show-help-overlay"));
        section_menu.append(Some(gettext("About RawValley").as_str()), Some("win.about"));
        menu.append_section(None, &section_menu);
        header.pack_end(
            &gtk::MenuButton::builder()
                .icon_name("open-menu-symbolic")
                .menu_model(&menu)
                .primary(true)
                .tooltip_text(gettext("Main Menu"))
                .build(),
        );

        let export_menu = export_menu("export", true);
        let export = adw::SplitButton::builder()
            .label(gettext("Export LUT"))
            .menu_model(&export_menu)
            .css_classes(["suggested-action"])
            .tooltip_text(gettext("Export a 33³ .cube (Ctrl+E)"))
            .sensitive(false)
            .build();
        let snd = sender.clone();
        export.connect_clicked(move |_| snd.input(Msg::Export(Format::Cube, 33, What::Grade)));
        header.pack_end(&export);

        let compare_btn = gtk::ToggleButton::builder()
            .icon_name("view-dual-symbolic")
            .tooltip_text(gettext("Before / after (\\)"))
            .build();
        let snd = sender.clone();
        compare_btn.connect_toggled(move |b| snd.input(Msg::Compare(b.is_active())));
        header.pack_end(&compare_btn);

        let sidebar_btn = gtk::ToggleButton::builder()
            .icon_name("sidebar-show-right-symbolic")
            .tooltip_text(gettext("Show panel"))
            .active(true)
            .visible(false)
            .build();
        grade_split.bind_property("show-sidebar", &sidebar_btn, "active").bidirectional().sync_create().build();
        header.pack_end(&sidebar_btn);

        let switcher_bar = adw::ViewSwitcherBar::builder().stack(&stack).build();
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&stack));
        toolbar.add_bottom_bar(&switcher_bar);
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&toolbar));
        window.set_content(Some(&toasts));

        // Narrow windows: overlay sidebars, switcher moves to the bottom bar.
        let bp = adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 900sp").expect("breakpoint"));
        bp.add_setter(&grade_split, "collapsed", Some(&true.to_value()));
        bp.add_setter(&lab.root, "collapsed", Some(&true.to_value()));
        bp.add_setter(&sidebar_btn, "visible", Some(&true.to_value()));
        bp.add_setter(&header, "title-widget", Some(&None::<gtk::Widget>.to_value()));
        bp.add_setter(&switcher_bar, "reveal", Some(&true.to_value()));
        window.add_breakpoint(bp);

        // ---------------- actions & shortcuts
        for (name, what) in [("export", What::Grade), ("lab-export", What::Lab)] {
            let action = gio::SimpleAction::new(name, Some(glib::VariantTy::STRING));
            let snd = sender.clone();
            action.connect_activate(move |_, v| {
                // Target "<kind>-<size>", e.g. "vlt-17", "identity-64".
                let Some(target) = v.and_then(|v| v.str().map(str::to_owned)) else { return };
                let Some((kind, size)) = target.split_once('-') else { return };
                let size = size.parse().unwrap_or(33);
                let (format, what) = match kind {
                    "vlt" => (Format::Vlt, what),
                    "hald" => (Format::Hald, what),
                    "identity" => (Format::Hald, What::Identity),
                    _ => (Format::Cube, what),
                };
                snd.input(Msg::Export(format, size, what));
            });
            window.add_action(&action);
        }
        for (name, which) in [("lab-add", Layers::Lab), ("look-add", Layers::Look)] {
            let action = gio::SimpleAction::new(name, Some(glib::VariantTy::STRING));
            let snd = sender.clone();
            action.connect_activate(move |_, v| {
                let add = match v.and_then(|v| v.str().map(str::to_owned)).as_deref() {
                    Some("selected") => LabAdd::Selected,
                    Some("file") => LabAdd::File,
                    Some("grade") => LabAdd::Grade,
                    _ => LabAdd::Convert,
                };
                snd.input(Msg::LayerAdd(which, add));
            });
            window.add_action(&action);
        }
        let background =
            gio::SimpleAction::new_stateful("background", Some(glib::VariantTy::STRING), &"default".to_variant());
        let canvases = [canvas.clone(), lab.canvas.clone()];
        background.connect_change_state(move |a, v| {
            let Some(name) = v.and_then(|v| v.str()).filter(|n| ["default", "black", "white"].contains(n)) else {
                return;
            };
            a.set_state(&name.to_variant());
            for c in &canvases {
                c.remove_css_class("bg-black");
                c.remove_css_class("bg-white");
                if name != "default" {
                    c.add_css_class(&format!("bg-{name}"));
                }
            }
            // ponytail: a failed save only forgets the choice.
            let _ = settings::set_value(settings::BACKGROUND, name);
        });
        if let Some(saved) = settings::value(settings::BACKGROUND) {
            background.change_state(&saved.to_variant());
        }
        window.add_action(&background);
        let about = gio::SimpleAction::new("about", None);
        let w = window.clone();
        about.connect_activate(move |_, _| {
            adw::AboutDialog::builder()
                .application_name("RawValley")
                .application_icon(APP_ID)
                .version(env!("CARGO_PKG_VERSION"))
                .comments(gettext(
                    "Color grading from photos and 3D .cube LUT creation, GPU accelerated (Vulkan/Metal).",
                ))
                .license_type(gtk::License::Gpl30)
                .developer_name("Nicolò Santamaria")
                .build()
                .present(Some(&w));
        });
        window.add_action(&about);
        let prefs_action = gio::SimpleAction::new("preferences", None);
        let snd = sender.clone();
        prefs_action.connect_activate(move |_, _| snd.input(Msg::Preferences));
        window.add_action(&prefs_action);
        let prefs_btn = gtk::Button::builder()
            .icon_name("folder-open-symbolic")
            .tooltip_text(gettext("Choose folder"))
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        let snd = sender.clone();
        prefs_btn.connect_clicked(move |_| snd.input(Msg::ChooseFolder));
        let prefs_row = adw::ActionRow::builder()
            .use_markup(false)
            .title(gettext("LUT folder"))
            .subtitle(gettext("No folder"))
            .activatable_widget(&prefs_btn)
            .build();
        prefs_row.add_suffix(&prefs_btn);
        let prefs_group = adw::PreferencesGroup::builder()
            .title(gettext("LUT library"))
            .description(gettext("LUT files in this folder and its subfolders appear in the LUT browser."))
            .build();
        prefs_group.add(&prefs_row);
        let labels: Vec<String> = PreviewQuality::ALL.map(PreviewQuality::label).to_vec();
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
        let quality_row = adw::ComboRow::builder()
            .title(gettext("Preview while editing"))
            .subtitle(gettext(
                "Faster previews are softer while you drag; the full-quality frame follows when you stop.",
            ))
            .model(&gtk::StringList::new(&labels))
            .selected(PreviewQuality::ALL.iter().position(|q| *q == PreviewQuality::stored()).unwrap_or(1) as u32)
            .build();
        let snd = sender.clone();
        quality_row.connect_selected_notify(move |r| snd.input(Msg::Quality(r.selected())));
        let view_group = adw::PreferencesGroup::builder().title(gettext("RAW development")).build();
        view_group.add(&quality_row);
        let camera_row = adw::SwitchRow::builder()
            .title(gettext("Open RAWs as in camera"))
            .subtitle(gettext(
                "A RAW you have not edited yet starts from the camera's white balance, exposure and look.",
            ))
            .active(settings::value(settings::AS_IN_CAMERA).as_deref() == Some("1"))
            .build();
        let snd = sender.clone();
        camera_row.connect_active_notify(move |r| snd.input(Msg::AsInCameraOnOpen(r.is_active())));
        view_group.add(&camera_row);
        let prefs_page =
            adw::PreferencesPage::builder().title(gettext("General")).icon_name("preferences-system-symbolic").build();
        prefs_page.add(&prefs_group);
        prefs_page.add(&view_group);
        let prefs = adw::PreferencesDialog::new();
        prefs.add(&prefs_page);
        let help = gio::SimpleAction::new("show-help-overlay", None);
        let w = window.clone();
        help.connect_activate(move |_, _| shortcuts_dialog().present(Some(&w)));
        window.add_action(&help);

        let shortcuts = gtk::ShortcutController::new();
        shortcuts.set_scope(gtk::ShortcutScope::Managed);
        let shortcut = |accel: &str, f: Box<dyn Fn() + 'static>| {
            shortcuts.add_shortcut(gtk::Shortcut::new(
                gtk::ShortcutTrigger::parse_string(accel),
                Some(gtk::CallbackAction::new(move |_, _| {
                    f();
                    glib::Propagation::Stop
                })),
            ));
        };
        let snd = sender.clone();
        shortcut("<Control>o", Box::new(move || snd.input(Msg::Open)));
        let e = export.clone();
        shortcut("<Control>e", Box::new(move || e.emit_clicked()));
        let c = chart_btn.clone();
        shortcut("t", Box::new(move || c.set_active(!c.is_active())));
        let c = compare_btn.clone();
        shortcut("backslash", Box::new(move || c.set_active(!c.is_active())));
        for (accel, b) in [("c", clip_btn.clone()), ("g", grid_btn.clone()), ("k", sidebar.skin_btn.clone())] {
            shortcut(accel, Box::new(move || b.set_active(b.is_sensitive() && !b.is_active())));
        }
        let snd = sender.clone();
        shortcut("<Control>s", Box::new(move || snd.input(Msg::PresetSave)));
        let s = stack.clone();
        shortcut("<Alt>1", Box::new(move || s.set_visible_child_name("grade")));
        let s = stack.clone();
        shortcut("<Alt>2", Box::new(move || s.set_visible_child_name("lab")));
        let (s, b, e) = (stack.clone(), sidebar.browse_btn.clone(), search.clone());
        shortcut(
            "<Control>f",
            Box::new(move || {
                s.set_visible_child_name("grade");
                b.set_active(true);
                e.grab_focus();
            }),
        );
        let snd = sender.clone();
        shortcut("F5", Box::new(move || snd.input(Msg::Reload)));
        let b = sidebar.browse_btn.clone();
        shortcut("b", Box::new(move || b.set_active(!b.is_active())));
        let w = window.clone();
        let snd = sender.clone();
        shortcut("<Control>comma", Box::new(move || snd.input(Msg::Preferences)));
        let c = crop_ui.edit.clone();
        shortcut("r", Box::new(move || c.set_active(!c.is_active())));
        let snd = sender.clone();
        shortcut("Escape", Box::new(move || snd.input(Msg::Escape)));
        let snd = sender.clone();
        shortcut("Return", Box::new(move || snd.input(Msg::CropEdit(false))));
        for (accel, cmd) in [
            ("<Control>0", ZoomCmd::Fit),
            ("<Control>1", ZoomCmd::Actual),
            ("<Control>plus", ZoomCmd::In),
            ("<Control>equal", ZoomCmd::In),
            ("<Control>KP_Add", ZoomCmd::In),
            ("<Control>minus", ZoomCmd::Out),
            ("<Control>KP_Subtract", ZoomCmd::Out),
        ] {
            let snd = sender.clone();
            shortcut(accel, Box::new(move || snd.input(Msg::Zoom(cmd))));
        }
        shortcut("<Control>question", Box::new(move || shortcuts_dialog().present(Some(&w))));
        window.add_controller(shortcuts);

        // Pending edits are written before the window goes.
        let snd = sender.clone();
        window.connect_close_request(move |_| {
            snd.input(Msg::Close);
            glib::Propagation::Stop
        });

        // Drag & drop a photo anywhere on the window.
        let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        let snd = sender.clone();
        drop.connect_drop(move |_, v, _, _| {
            let Ok(list) = v.get::<gdk::FileList>() else { return false };
            let paths: Vec<PathBuf> = list.files().iter().filter_map(|f| f.path()).collect();
            if paths.is_empty() {
                return false;
            }
            snd.input(Msg::RollAddPaths(paths));
            true
        });
        window.add_controller(drop);

        let model = App {
            grade: Grade::default(),
            band: 0,
            compare: false,
            split: 0.5,
            path: None,
            luts: Vec::new(),
            lut_paths: Arc::default(),
            group_names: Vec::new(),
            group: None,
            query: String::new(),
            page: Page::Grade,
            lab_layers: Vec::new(),
            looks: Vec::new(),
            bypassed: Vec::new(),
            looks_off: false,
            browsing: false,
            candidate: None,
            preview_req: None,
            hover_tick: 0,
            flash_new_look: false,
            base_key: None,
            gallery_stale: true,
            lab_input: Profile::Standard,
            lab_blend: Blend::Chain,
            look_blend: Blend::Chain,
            monitors: Vec::new(),
            folder_changes: 0,
            profile: Profile::Standard,
            decoded: None,
            photo: None,
            chart: None,
            show_chart: false,
            selected: None,
            jobs,
            sent: None,
            load_gen: 0,
            gallery_gen: 0,
            overlay: false,
            roll: Vec::new(),
            active: None,
            roll_gen: 0,
            roll_sent: None,
            roll_tick: 0,
            match_file: false,
            match_gen: 0,
            input: sender.input_sender().clone(),
            crop: Crop::default(),
            ratio: Ratio::Free,
            editing: None,
            zoom: None,
            canvas: [1.0, 1.0],
            view_gen: 0,
            encoding: false,
            develop: Develop::default(),
            quality: PreviewQuality::stored(),
            drafting: false,
            develop_tick: 0,
            camera_match: None,
            camera_job: None,
            match_parts: (true, true),
            match_undo: None,
            saved: None,
            save_tick: 0,
            looks_loading: false,
            save_failed: false,
            encode_again: false,
            encoded: None,
            ui: Ui {
                window: window.clone(),
                toasts,
                grade_stack,
                grade_pic,
                info,
                sidebar,
                gallery_stack,
                flow,
                folder_label,
                groups,
                prefs,
                prefs_row,
                lab,
                stack: stack.clone(),
                thumbs: Vec::new(),
                export,
                chart_btn,
                camera_btn,
                presets,
                filmstrip,
                canvas_stack,
                grid_btn,
                legend,
                zoom,
                crop: crop_ui,
            },
        };
        OPEN.with(|o| o.set(sender.input_sender().clone()).ok());
        if let Some(dir) = settings::folder(settings::LUT_FOLDER) {
            sender.input(Msg::Folder(dir));
        }
        ComponentParts { model, widgets: () }
    }

    fn update(&mut self, msg: Msg, sender: ComponentSender<Self>) {
        match msg {
            Msg::Open => {
                // add_suffix matches case-insensitively.
                let dialog = photo_dialog(&gettext("Open Photo"));
                let s = sender.clone();
                dialog.open_multiple(Some(&self.ui.window), gio::Cancellable::NONE, move |r| {
                    let Ok(files) = r else { return };
                    let paths: Vec<PathBuf> =
                        files.iter::<gio::File>().filter_map(|f| f.ok().and_then(|f| f.path())).collect();
                    if !paths.is_empty() {
                        s.input(Msg::RollAddPaths(paths));
                    }
                });
            }
            Msg::RollAddPaths(paths) => {
                // Already in the strip: just show it. New ones are appended, the
                // first of them is opened, and all are decoded small in one thread.
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
                            let grade = Sidecar::load(&p).ok().flatten().map(|sc| sc.grade).unwrap_or_default();
                            self.roll.push(RollItem { path: p.clone(), grade, small: None, encoded: None });
                            fresh.push(p);
                        }
                        None => {
                            self.toast(&gettext!("The filmstrip holds up to {} photos", MAX_ROLL));
                            break;
                        }
                    }
                }
                let open = show.filter(|&i| Some(i) != self.active || !self.has_source());
                if !fresh.is_empty() {
                    self.roll_changed(&sender);
                    // The photo about to be opened gets its small copy from that decode.
                    let opening = open.map(|i| self.roll[i].path.clone());
                    fresh.retain(|p| Some(p) != opening.as_ref());
                    let (s, profile) = (sender.input_sender().clone(), self.profile);
                    std::thread::spawn(move || {
                        for p in fresh {
                            let small = Decoded::load_max(&p, Some(ROLL_PX)).map(|d| roll_copy(&d, profile));
                            s.emit(Msg::RollLoaded(p, small));
                        }
                    });
                }
                if let Some(i) = open {
                    self.load_active(i, &sender);
                }
            }
            Msg::RollLoaded(path, Ok((small, profile, photo))) => {
                if let Some(it) = self.roll.iter_mut().find(|it| it.path == path) {
                    (it.small, it.encoded) = (Some(small), Some((profile, photo)));
                    self.upload_roll(false);
                    // Decoded while the profile changed: show it in the new one.
                    if profile != self.profile {
                        self.reencode_roll();
                    }
                }
            }
            Msg::RollEncoded(profile, photos) => {
                for (path, photo) in photos {
                    if let Some(it) = self.roll.iter_mut().find(|it| it.path == path) {
                        it.encoded = Some((profile, photo));
                    }
                }
                self.upload_roll(false);
            }
            Msg::RollTick(tick) if tick == self.roll_tick => {
                let grades = self.roll_grades();
                if !self.roll.is_empty() && self.roll_sent.as_ref() != Some(&grades) {
                    self.send(Job::RenderRoll(grades.clone(), self.active));
                    self.roll_sent = Some(grades);
                }
                self.refresh_gallery();
            }
            Msg::RollTick(_) => {}
            Msg::RollLoaded(path, Err(e)) => {
                // An unreadable photo leaves the strip instead of staying a black card.
                if let Some(i) = self.roll.iter().position(|it| it.path == path) {
                    self.drop_from_roll(i, &sender);
                }
                self.toast(&gettext!("Cannot open {}: {}", path.display(), e));
            }
            Msg::RollSelect(i) => {
                if Some(i) != self.active {
                    self.load_active(i, &sender);
                }
            }
            Msg::RollRemove(i) => {
                if i >= self.roll.len() {
                    return;
                }
                let was_active = self.drop_from_roll(i, &sender);
                if was_active && self.roll.is_empty() {
                    // Nothing left to grade: back to the empty state; a decode in flight is stale.
                    self.load_gen += 1;
                    self.cancel_match();
                    (self.decoded, self.photo, self.path) = (None, None, None);
                    self.ui.window.set_title(Some("RawValley"));
                    self.show_source();
                } else if was_active {
                    self.load_active(i.min(self.roll.len() - 1), &sender);
                }
            }
            Msg::Grid(on) => {
                self.ui.canvas_stack.set_visible_child_name(if on { "grid" } else { "single" });
                if self.ui.grid_btn.is_active() != on {
                    self.ui.grid_btn.set_active(on);
                }
            }
            Msg::Develop(edit) => self.edit_develop(edit, &sender),
            Msg::SaveSidecar(tick) if tick == self.save_tick => self.save_now(),
            Msg::SaveSidecar(_) => {}
            Msg::Close => {
                self.save_now();
                self.ui.window.destroy();
            }
            // Another photo was opened meanwhile: these looks are not its own.
            Msg::SidecarLooks(path, _) if path != self.path => {}
            Msg::SidecarLooks(_, loaded) => {
                let missing = loaded.iter().filter(|(_, l)| l.is_err()).count();
                for (r, lut) in loaded {
                    if let Ok(lut) = lut {
                        self.looks.push(LabLayer {
                            kind: LabKind::Lut(lut),
                            intensity: r.intensity,
                            input: r.input,
                            on: r.on,
                            path: Some(r.path),
                        });
                    }
                }
                if missing > 0 {
                    self.toast(&gettext!("{} LUTs of this photo are missing", missing));
                }
                self.layers_changed(Layers::Look, true, &sender);
                self.looks_loading = false;
                self.saved = Some(self.sidecar_state());
            }
            Msg::AsInCamera(part) => self.as_in_camera(part, &sender),
            Msg::CameraJpeg(path, _) if Some(&path) != self.path.as_ref() => {}
            Msg::CameraJpeg(path, Ok(reference)) => {
                let (Some(d), Some(cam)) = (self.decoded.clone(), self.camera().cloned()) else { return };
                // The source is the photo as the camera shot it, before any edit.
                let (w, h) = d.size();
                let whole = Region { center: [w as f32 / 2.0, h as f32 / 2.0], size: [w as f32, h as f32], angle: 0.0 };
                let as_shot = Develop { wb: None, exposure: cam.baseline_ev, ..self.develop };
                let source = d.encode_fit(&whole, 1024, self.profile, &as_shot);
                let m = &self.ui.sidebar.matching;
                m.pic.set_paintable(Some(&texture(reference.width, reference.height, rgba8(&reference.pixels))));
                m.name.set_label(&gettext("Camera JPEG"));
                self.match_gen += 1;
                self.camera_job = Some(path);
                let input = self.grade.neutralized();
                self.send(Job::Match { generation: self.match_gen, source, reference, input });
            }
            Msg::CameraJpeg(_, Err(_)) => {
                self.show_match_panel();
                self.toast(&gettext("This RAW has no camera JPEG to match"));
            }
            Msg::AsInCameraOnOpen(on) => {
                if let Err(e) = settings::set_value(settings::AS_IN_CAMERA, if on { "1" } else { "0" }) {
                    self.toast(&gettext!("Cannot save the setting: {}", e));
                }
            }
            Msg::CanvasClick(p) => {
                if let Some(at) = self.image_point(p).filter(|_| self.ui.sidebar.develop.picker.is_active()) {
                    self.send(Job::SampleRaw { at, radius: 4 });
                }
            }
            Msg::DevelopSettled(tick) if tick == self.develop_tick && self.drafting => {
                self.drafting = false;
                self.encode_photo(&sender);
                self.request_view();
            }
            Msg::DevelopSettled(_) => {}
            Msg::Quality(i) => {
                self.quality = PreviewQuality::ALL[i as usize % 3];
                if let Err(e) = settings::set_value(settings::PREVIEW_QUALITY, self.quality.key()) {
                    self.toast(&gettext!("Cannot save the setting: {}", e));
                }
                self.encode_photo(&sender);
            }
            // Superseded: not loaded on the GPU, but the strip still needs its copy
            // (RollAddPaths left it to this load).
            Msg::RawRead(generation, path, _) if generation != self.load_gen => {
                let (s, profile) = (sender.input_sender().clone(), self.profile);
                std::thread::spawn(move || {
                    let small = Decoded::load_max(&path, Some(ROLL_PX)).map(|d| roll_copy(&d, profile));
                    s.emit(Msg::RollLoaded(path, small));
                });
            }
            Msg::RawRead(generation, path, Ok(raw)) => self.send(Job::LoadRaw(generation, path, raw)),
            Msg::RawRead(generation, path, Err(e)) => {
                sender.input(Msg::RollLoaded(path.clone(), Err(e.clone())));
                sender.input(Msg::Loaded(generation, path, Err(e)));
            }
            Msg::Loaded(generation, ..) if generation != self.load_gen => {}
            Msg::Loaded(_, path, Ok(decoded)) => {
                self.save_now(); // the previous photo's last edits
                // Each photo has its own grade and looks: the previous one keeps its frame's.
                let previous = self.path.as_ref().and_then(|p| self.roll.iter_mut().find(|it| it.path == *p));
                if let Some(it) = previous {
                    it.grade = self.grade.clone();
                }
                self.apply_grade(Grade::default());
                self.looks.clear();
                self.ui.sidebar.look_blend_toggle.set_active_name(Some("chain"));
                self.layers_changed(Layers::Look, true, &sender);
                self.ui.sidebar.develop.picker.set_active(false);
                self.ui.window.set_title(Some(&format!("{} — RawValley", file_label(&path))));
                self.zoom = None;
                self.reset_crop();
                self.decoded = Some(decoded);
                self.path = Some(path);
                self.develop = Develop::default();
                self.sync_develop();
                self.looks_loading = false;
                let path = self.path.clone().expect("set above");
                match Sidecar::load(&path) {
                    Ok(Some(sc)) => self.restore_sidecar(sc, &sender),
                    Ok(None) => {
                        if self.camera().is_some() && settings::value(settings::AS_IN_CAMERA).as_deref() == Some("1") {
                            sender.input(Msg::AsInCamera(CameraPart::All));
                        }
                    }
                    Err(e) => self.toast(&gettext!("Cannot read the edits saved for {}: {}", file_label(&path), e)),
                }
                // Nothing is written until something changes from what was just read.
                self.saved = Some(self.sidecar_state());
                // Opening a photo means you want to see it.
                if self.show_chart {
                    self.show_chart = false;
                    self.ui.chart_btn.set_active(false);
                }
                self.encode_photo(&sender);
            }
            // RollLoaded already reported the error and took the photo out of the
            // strip; the photo still on screen (if any) stays the one edited.
            Msg::Loaded(_, _, Err(_)) => {
                self.active = self.path.as_ref().and_then(|p| self.roll.iter().position(|it| it.path == *p));
                self.ui.filmstrip.set_active(self.active);
                self.show_source();
            }
            Msg::Encoded(profile, photo) => {
                self.encoding = false;
                if std::mem::take(&mut self.encode_again) {
                    self.encode_photo(&sender);
                } else if profile == self.profile {
                    self.photo = Some((photo, profile));
                    self.show_source();
                }
            }
            Msg::Set(field, v) => {
                *field(&mut self.grade.params) = v as f32;
                self.render();
            }
            Msg::Wheel(i, x, y) => {
                let w = wheel_mut(&mut self.grade.params, i);
                (w[0], w[1]) = (x, y);
                self.render();
            }
            Msg::WheelLuma(i, v) => {
                wheel_mut(&mut self.grade.params, i)[2] = v as f32;
                self.render();
            }
            Msg::Band(b) => self.show_band(b),
            Msg::Hue(k, v) => {
                let p = &mut self.grade.params;
                let band = match k {
                    0 => &mut p.hue_shift,
                    1 => &mut p.hue_sat,
                    _ => &mut p.hue_lum,
                };
                band[self.band] = v as f32;
                self.render();
            }
            Msg::Curve(ch, c) => {
                self.grade.curves[ch] = c;
                self.render();
            }
            Msg::ResetAll => {
                let before = self.sidecar_state();
                if self.develop != Develop::default() {
                    self.develop = Develop::default();
                    self.sync_develop();
                    self.develop_changed(&sender);
                }
                self.apply_grade(Grade::default());
                self.reset_crop();
                self.crop_changed(&sender);
                self.looks.clear();
                self.ui.sidebar.look_blend_toggle.set_active_name(Some("chain"));
                self.layers_changed(Layers::Look, true, &sender);
                self.undo_toast(&gettext("All edits cleared"), move || Msg::RestoreEdits(Box::new(before.clone())));
            }
            Msg::RestoreEdits(sc) => {
                let develop = self.develop;
                // Only onto a cleared photo, so its grade and looks come back too.
                self.apply_grade(Grade::default());
                self.looks.clear();
                self.restore_sidecar(*sc, &sender);
                if self.develop != develop {
                    self.develop_changed(&sender);
                }
                self.crop_changed(&sender);
                self.layers_changed(Layers::Look, true, &sender);
            }
            Msg::RestoreGrade(grade) => self.apply_grade(*grade),
            Msg::MixerMono(on) => {
                self.grade.params.mixer_mono = if on { 1.0 } else { 0.0 };
                let m = &self.ui.sidebar.mixer;
                m.rows.set_sensitive(!on);
                let page = m.rows.active_name().map(|n| n.to_string()).unwrap_or_else(|| "0".into());
                m.pages.set_visible_child_name(if on { "mono" } else { &page });
                self.render();
            }
            Msg::MixerFree(free) => {
                self.grade.params.mixer_free = if free { 1.0 } else { 0.0 };
                self.render();
            }
            Msg::Mixer(i, delta) => {
                *mixer_value_mut(&mut self.grade.params, i) = delta as f32;
                self.render();
            }
            Msg::MatchChoose => {
                if !self.has_source() {
                    return self.toast(&no_photo_to_match());
                }
                let dialog = photo_dialog(&gettext("Choose Reference"));
                let s = sender.clone();
                dialog.open(Some(&self.ui.window), gio::Cancellable::NONE, move |r| {
                    if let Some(p) = r.ok().and_then(|f| f.path()) {
                        s.input(Msg::MatchReference(p));
                    }
                });
            }
            Msg::MatchReference(path) => {
                self.ui.sidebar.matching.stack.set_visible_child_name("busy");
                let s = sender.input_sender().clone();
                std::thread::spawn(move || {
                    // References are display images (a still, a JPEG); RAWs are rendered neutrally.
                    let r = Decoded::load_max(&path, Some(1024)).map(|d| {
                        let reference = d.encode(Profile::Standard, &Develop::default());
                        let thumb = rgba8(&reference.pixels);
                        (reference, thumb)
                    });
                    s.emit(Msg::MatchLoaded(path, r));
                });
            }
            Msg::MatchLoaded(path, Ok((reference, thumb))) => {
                let m = &self.ui.sidebar.matching;
                m.pic.set_paintable(Some(&texture(reference.width, reference.height, thumb)));
                m.name.set_label(&file_label(&path));
                self.match_file = true;
                self.request_match(reference);
            }
            Msg::MatchLoaded(path, Err(e)) => {
                self.show_match_panel();
                self.toast(&gettext!("Cannot open {}: {}", path.display(), e));
            }
            Msg::MatchClear => {
                self.match_file = false;
                self.grade.bypass(Stage::Match);
                self.ui.sidebar.matching.amount.adj.set_value(0.0);
                self.cancel_match();
                self.render();
            }
            Msg::Overlay(on) => {
                self.overlay = on;
                self.ui.legend.set_reveal_child(on);
                self.render();
            }
            Msg::SkinLine(on) => self.ui.sidebar.skin.set_visible(on),
            Msg::PresetsRefresh => {
                let list: Vec<(String, PathBuf)> = self.presets().into_iter().map(|(p, path)| (p.name, path)).collect();
                self.ui.presets.fill(&list, &sender);
            }
            Msg::PresetSave => {
                let suggestion = self.path.as_deref().and_then(stem_label).unwrap_or_default();
                presets_ui::ask_name(&self.ui.window, &suggestion, &sender);
            }
            // What you see is what is saved: switched-off sections stay off.
            Msg::PresetSaveAs(name) => {
                sender.input(Msg::PresetWrite(Box::new(self.current_preset(name)), false, false))
            }
            Msg::PresetWrite(preset, force, apply) => {
                let path = settings::presets_dir().join(preset::file_name(&preset.name));
                let existing = self.presets();
                if !force && existing.iter().any(|(p, at)| *at == path || p.name == preset.name) {
                    let name = preset.name.clone();
                    let replace = move || Msg::PresetWrite(preset.clone(), true, apply);
                    return presets_ui::confirm_replace(&self.ui.window, &name, replace, &sender);
                }
                // A preset with the same name in another file is replaced too.
                for (p, at) in existing {
                    if p.name == preset.name && at != path {
                        let _ = std::fs::remove_file(at);
                    }
                }
                match preset.save(&path) {
                    Ok(()) if apply => sender.input(Msg::PresetApply(path)),
                    Ok(()) => self.toast(&gettext!("Preset “{}” saved", preset.name)),
                    Err(e) => self.toast(&gettext!("Cannot save the preset: {}", e)),
                }
            }
            Msg::PresetApply(path) => match Preset::load(&path) {
                Ok(p) => {
                    let before = self.grade.clone();
                    self.match_file = false;
                    self.apply_grade(p.grade);
                    self.undo_toast(&gettext!("Preset “{}” applied", p.name), move || {
                        Msg::RestoreGrade(Box::new(before.clone()))
                    });
                }
                Err(e) => self.toast(&gettext!("Cannot read the preset: {}", e)),
            },
            Msg::PresetDelete(path) => {
                // Deleting is immediate; the toast offers to put it back.
                let Ok(text) = std::fs::read_to_string(&path) else { return };
                if let Err(e) = std::fs::remove_file(&path) {
                    return self.toast(&gettext!("Cannot delete the preset: {}", e));
                }
                sender.input(Msg::PresetsRefresh);
                let name = Preset::from_json(&text).map(|p| p.name).unwrap_or_default();
                self.undo_toast(&gettext!("Preset “{}” deleted", name), move || {
                    Msg::PresetRestore(path.clone(), text.clone())
                });
            }
            Msg::PresetRestore(path, text) => {
                if let Err(e) = std::fs::write(&path, text) {
                    self.toast(&gettext!("Cannot restore the preset: {}", e));
                }
                sender.input(Msg::PresetsRefresh);
            }
            Msg::PresetImport => {
                let dialog = preset_dialog(&gettext("Import Preset"));
                let s = sender.clone();
                dialog.open(Some(&self.ui.window), gio::Cancellable::NONE, move |r| {
                    let Some(p) = r.ok().and_then(|f| f.path()) else { return };
                    match Preset::load(&p) {
                        // Imported presets join the list under their own name, asking before replacing one.
                        Ok(preset) => s.input(Msg::PresetWrite(Box::new(preset), false, true)),
                        Err(e) => s.input(Msg::Toast(gettext!("Not a valid preset: {}", e))),
                    }
                });
            }
            Msg::PresetExport => {
                let dialog = preset_dialog(&gettext("Export Preset"));
                let stem = self.path.as_deref().and_then(stem_label).unwrap_or_else(|| "preset".into());
                dialog.set_initial_name(Some(&preset::file_name(&stem)));
                let s = sender.clone();
                dialog.save(Some(&self.ui.window), gio::Cancellable::NONE, move |r| {
                    if let Some(p) = r.ok().and_then(|f| f.path()) {
                        s.input(Msg::PresetExportTo(p));
                    }
                });
            }
            Msg::PresetExportTo(path) => match self.current_preset(stem_label(&path).unwrap_or_default()).save(&path) {
                Ok(()) => self.toast(&gettext!("Preset saved: {}", path.display())),
                Err(e) => self.toast(&gettext!("Cannot save the preset: {}", e)),
            },
            Msg::Toast(text) => self.toast(&text),
            Msg::Compare(on) => {
                self.compare = on;
                self.refresh();
            }
            Msg::Split(x) => {
                if self.compare {
                    self.split = x;
                    self.refresh();
                }
            }
            // Ctrl+E fires even while the button is insensitive.
            Msg::Export(_, _, What::Grade) if !self.has_source() => {}
            Msg::Export(_, _, What::Lab) if self.lab_layers.is_empty() => {}
            Msg::Export(format, size, what) => {
                let stem = match (what, &self.path) {
                    (What::Identity, _) => format!("hald_identity_{size}"),
                    (What::Lab, _) => format!("lab_{}_{size}", self.lab_input.tag()),
                    (What::Grade, Some(p)) => {
                        let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                        format!("{stem}_{}_{size}", self.profile.tag())
                    }
                    (What::Grade, None) => format!("chart_{}_{size}", self.profile.tag()),
                };
                let label = format!("{} {size}³", format.badge());
                let dialog = gtk::FileDialog::builder()
                    .title(gettext!("Export {}", label))
                    .initial_name(format!("{stem}.{}", format.ext()))
                    .filters(&file_filters(&label, &[format.ext().into()]))
                    .modal(true)
                    .build();
                let s = sender.clone();
                dialog.save(Some(&self.ui.window), gio::Cancellable::NONE, move |r| {
                    if let Some(p) = r.ok().and_then(|f| f.path()) {
                        s.input(Msg::ExportTo(format, size, what, p));
                    }
                });
            }
            Msg::ExportTo(format, size, what, path) => {
                let title = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                match what {
                    What::Identity => {
                        // The neutral LUT needs no GPU: write it off the main thread.
                        let s = sender.input_sender().clone();
                        std::thread::spawn(move || {
                            let r = Lut3d::identity(size, format).save(&path).map(|_| path);
                            s.emit(Msg::Gpu(Done::Exported(r)));
                        });
                    }
                    What::Grade => self.send(Job::Export {
                        grade: self.graded(),
                        look: self.look_stack(self.look_layers()),
                        size,
                        format,
                        title,
                        path,
                    }),
                    What::Lab => {
                        self.send(Job::ExportLab { stack: self.lab_stack(self.lab_input), size, format, title, path })
                    }
                }
            }
            Msg::ChooseFolder => {
                let dialog = gtk::FileDialog::builder().title(gettext("LUT folder")).modal(true).build();
                dialog.set_initial_folder(start_in(settings::LUT_FOLDER).as_ref());
                let s = sender.clone();
                dialog.select_folder(Some(&self.ui.window), gio::Cancellable::NONE, move |r| {
                    if let Some(p) = r.ok().and_then(|f| f.path()) {
                        s.input(Msg::Folder(p));
                    }
                });
            }
            Msg::Folder(dir) => {
                if let Err(e) = settings::set_folder(settings::LUT_FOLDER, &dir) {
                    self.toast(&gettext!("Cannot save the LUT folder: {}", e));
                }
                self.ui.prefs_row.set_subtitle(&dir.to_string_lossy());
                self.ui
                    .folder_label
                    .set_label(&dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
                // Watched before the scan, so files added while it runs trigger another one.
                self.watch_folders(&sender);
                let s = sender.input_sender().clone();
                std::thread::spawn(move || {
                    let (luts, errors, truncated) = Lut3d::load_dir(&dir);
                    s.emit(Msg::GalleryLoaded(luts, errors, truncated));
                });
            }
            Msg::GalleryLoaded(luts, errors, truncated) => {
                if !errors.is_empty() {
                    self.toast(&gettext!("{} files skipped: {}", errors.len(), errors.join("; ")));
                }
                // A reload keeps what the user was looking at, when it still exists.
                let path_of = |i: usize| self.luts.get(i).map(|l| l.path.clone());
                let keep_selected = self.selected.and_then(path_of);
                let keep_candidate = self.candidate.as_ref().and_then(|c| path_of(c.0));
                let keep_group = self.group.take();
                let flow = self.ui.flow.clone();
                let had_focus = flow.root().and_then(|r| r.focus()).is_some_and(|f| f.is_ancestor(&flow));
                if had_focus {
                    // Focus falling back into an emptied FlowBox selects its first new card: park it.
                    self.ui.sidebar.browse_btn.grab_focus();
                }
                self.ui.flow.remove_all();
                self.ui.thumbs.clear();
                if truncated {
                    self.toast(&gettext!("Very large library: showing the first {} LUTs", MAX_LIBRARY));
                }
                for (i, lut) in luts.iter().enumerate() {
                    let pic = gtk::Picture::builder().content_fit(gtk::ContentFit::Cover).height_request(64).build();
                    let badge = gtk::Label::builder()
                        .label(lut.format.badge())
                        .halign(gtk::Align::End)
                        .valign(gtk::Align::Start)
                        .css_classes(["lut-badge"])
                        .build();
                    let framed = gtk::Overlay::builder().child(&pic).build();
                    framed.add_overlay(&badge);
                    let card = vbox(4);
                    card.add_css_class("thumb-card");
                    card.append(&framed);
                    card.append(
                        &gtk::Label::builder()
                            .label(&lut.title)
                            .ellipsize(gtk::pango::EllipsizeMode::End)
                            .css_classes(["caption"])
                            .build(),
                    );
                    let place = if lut.group.is_empty() { String::new() } else { format!("{} · ", lut.group) };
                    card.set_tooltip_text(Some(&format!(
                        "{place}{} · {} {}³",
                        lut.title,
                        lut.format.badge(),
                        lut.size
                    )));
                    let motion = gtk::EventControllerMotion::new();
                    let s = sender.input_sender().clone();
                    motion.connect_enter(move |_, _, _| s.emit(Msg::Hover(Some(i))));
                    card.add_controller(motion);
                    self.ui.flow.append(&card);
                    self.ui.thumbs.push(pic);
                }
                self.ui.gallery_stack.set_visible_child_name(if luts.is_empty() { "empty" } else { "grid" });
                self.group = None;
                self.group_names = luts.iter().map(|l| l.group.clone()).collect();
                self.group_names.dedup(); // sorted by group in core
                let labels: Vec<String> = std::iter::once(gettext("All folders"))
                    .chain(
                        self.group_names.iter().map(|g| if g.is_empty() { gettext("Main folder") } else { g.clone() }),
                    )
                    .collect();
                let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
                self.ui.groups.set_model(Some(&gtk::StringList::new(&labels)));
                self.ui.groups.set_visible(self.group_names.iter().any(|g| !g.is_empty()));
                let group = keep_group.and_then(|g| self.group_names.iter().position(|n| *n == g));
                self.ui.groups.set_selected(group.map_or(0, |i| i as u32 + 1));
                self.lut_paths = Arc::new(luts.iter().map(|l| l.path.clone()).collect());
                self.luts = luts;
                self.filter_gallery();
                let find = |p: Option<PathBuf>| p.and_then(|p| self.luts.iter().position(|l| l.path == p));
                self.selected = find(keep_selected);
                match (find(keep_candidate), self.candidate.as_mut()) {
                    (Some(i), Some(c)) => c.0 = i,
                    _ => self.drop_candidate(),
                }
                let kept = self.selected.and_then(|i| flow.child_at_index(i as i32));
                if let Some(c) = &kept {
                    flow.select_child(c);
                }
                if had_focus {
                    kept.or_else(|| flow.child_at_index(0)).map_or_else(|| flow.grab_focus(), |c| c.grab_focus());
                }
                self.gallery_stale = true;
                self.refresh_gallery();
            }
            Msg::Group(sel) => {
                // Index 0 shows everything; otherwise only LUTs of that sub-folder.
                self.group = (sel as usize).checked_sub(1).and_then(|i| self.group_names.get(i)).cloned();
                self.filter_gallery();
                self.hide_filtered_selection();
            }
            Msg::Bypass(Some(stage), off) => {
                self.bypassed.retain(|&s| s != stage);
                if off {
                    self.bypassed.push(stage);
                }
                self.render();
            }
            Msg::Bypass(None, off) => {
                self.looks_off = off;
                self.layers_changed(Layers::Look, false, &sender);
            }
            Msg::Browse(on) => {
                self.browsing = on;
                self.ui.sidebar.browse.set_reveal_child(on);
                if self.ui.sidebar.browse_btn.is_active() != on {
                    self.ui.sidebar.browse_btn.set_active(on);
                }
                if !on {
                    self.drop_candidate();
                }
                self.refresh_gallery();
            }
            Msg::Reload => match settings::folder(settings::LUT_FOLDER) {
                Some(dir) => sender.input(Msg::Folder(dir)),
                None => sender.input(Msg::ChooseFolder),
            },
            Msg::Search(text) => {
                self.query = text.to_lowercase();
                self.filter_gallery();
                self.hide_filtered_selection();
            }
            Msg::Preferences => self.ui.prefs.present(Some(&self.ui.window)),
            Msg::SelectLut(i) => {
                if i < self.luts.len() {
                    self.selected = Some(i);
                    sender.input(Msg::Preview(Some(i)));
                }
            }
            Msg::Preview(None) => self.drop_candidate(),
            Msg::Preview(Some(i)) => {
                self.preview_req = Some(i);
                let Some(entry) = self.luts.get(i).filter(|_| self.candidate.as_ref().is_none_or(|c| c.0 != i)) else {
                    return;
                };
                let (p, s) = (entry.path.clone(), sender.input_sender().clone());
                std::thread::spawn(move || s.emit(Msg::PreviewLoaded(i, Lut3d::load(&p).map(Arc::new))));
            }
            // Only the latest request is shown; older loads that land late are dropped.
            Msg::PreviewLoaded(i, _) if self.preview_req != Some(i) => {}
            Msg::PreviewLoaded(i, Ok(lut)) => {
                self.candidate = Some((i, lut));
                self.show_looks();
            }
            Msg::PreviewLoaded(i, Err(e)) => {
                self.drop_candidate();
                let name = self.luts.get(i).map(|l| l.title.clone()).unwrap_or_default();
                self.toast(&gettext!("Cannot read {}: {}", name, e));
            }
            Msg::Hover(over) => {
                // A short rest before previewing, so sweeping across the grid does not flicker the photo.
                self.hover_tick += 1;
                let (s, tick) = (self.input.clone(), self.hover_tick);
                glib::timeout_add_local_once(HOVER_REST, move || s.emit(Msg::HoverSettled(tick, over)));
            }
            Msg::HoverSettled(tick, over) if tick == self.hover_tick => {
                sender.input(Msg::Preview(over.or(self.selected)));
            }
            Msg::HoverSettled(..) => {}
            Msg::Commit(i) => {
                self.flash_new_look = true;
                match self.candidate.take().filter(|c| c.0 == i) {
                    Some((_, lut)) => {
                        let path = self.luts.get(i).map(|l| l.path.clone());
                        self.push_lut(Layers::Look, lut, path, &sender)
                    }
                    None => {
                        if let Some(entry) = self.luts.get(i) {
                            Self::load_layer(Layers::Look, entry.path.clone(), &sender);
                        }
                    }
                }
                // Added: no longer a preview, so leaving the grid cannot bring it back on top of itself.
                self.clear_selection();
            }
            Msg::Escape => {
                if self.ui.sidebar.develop.picker.is_active() {
                    self.ui.sidebar.develop.picker.set_active(false);
                } else if self.editing.is_some() {
                    sender.input(Msg::CropCancel);
                } else if self.candidate.is_some() || self.preview_req.is_some() {
                    self.clear_selection();
                }
            }
            Msg::Profile(profile) => {
                self.profile = profile;
                self.reencode_roll();
                if self.show_chart {
                    sender.input(Msg::Chart(true));
                }
                self.encode_photo(&sender);
            }
            Msg::Chart(on) => {
                self.show_chart = on;
                // The chart is encoded per profile; (re)build it off the main thread.
                if !on || self.chart.as_ref().is_some_and(|c| c.1 == self.profile) {
                    self.show_source();
                } else {
                    let (s, profile) = (sender.input_sender().clone(), self.profile);
                    std::thread::spawn(move || s.emit(Msg::ChartReady(profile, Arc::new(test_chart(profile)))));
                }
            }
            Msg::ChartReady(profile, chart) => {
                if profile == self.profile {
                    self.chart = Some((chart, profile));
                    if self.show_chart {
                        self.show_source();
                    }
                }
            }
            Msg::LayerAdd(which, add) => match add {
                LabAdd::Selected => match self.selected.and_then(|i| self.luts.get(i)) {
                    Some(entry) => Self::load_layer(which, entry.path.clone(), &sender),
                    None => self.toast(&gettext("Select a LUT in the browser first")),
                },
                LabAdd::File => {
                    let pats: Vec<String> = ["cube", "vlt", "png"].map(String::from).to_vec();
                    let dialog = gtk::FileDialog::builder()
                        .title(gettext("Add LUT"))
                        .filters(&file_filters(&gettext("LUTs (.cube, .vlt, HALD .png)"), &pats))
                        .modal(true)
                        .build();
                    dialog.set_initial_folder(start_in(settings::LUT_FOLDER).as_ref());
                    let s = sender.clone();
                    dialog.open(Some(&self.ui.window), gio::Cancellable::NONE, move |r| {
                        if let Some(p) = r.ok().and_then(|f| f.path()) {
                            Self::load_layer(which, p, &s);
                        }
                    });
                }
                LabAdd::Grade => self.push_layer(which, LabKind::Grade, Profile::Standard, &sender),
                LabAdd::Convert => {
                    self.push_layer(which, LabKind::Convert(Profile::Standard), Profile::Standard, &sender)
                }
            },
            Msg::OpenInLab => {
                if self.selected.is_some() {
                    self.ui.stack.set_visible_child_name("lab");
                }
                sender.input(Msg::LayerAdd(Layers::Lab, LabAdd::Selected));
            }
            Msg::LayerLoaded(which, p, Ok(lut)) => self.push_lut(which, Arc::new(lut), Some(p), &sender),
            Msg::LayerLoaded(_, p, Err(e)) => {
                self.flash_new_look = false;
                self.toast(&gettext!("Cannot read {}: {}", p.display(), e))
            }
            Msg::LayerRemove(which, i) => {
                if i < self.layers_mut(which).len() {
                    self.layers_mut(which).remove(i);
                    self.layers_changed(which, true, &sender);
                }
            }
            Msg::LayerMove(which, i, up) => {
                let j = if up { i.checked_sub(1) } else { Some(i + 1) };
                if let Some(j) = j.filter(|&j| j < self.layers_mut(which).len()) {
                    self.layers_mut(which).swap(i, j);
                    self.layers_changed(which, true, &sender);
                }
            }
            Msg::LayerIntensity(which, i, v) => {
                if let Some(l) = self.layers_mut(which).get_mut(i) {
                    l.intensity = (v / 100.0) as f32;
                    self.layers_changed(which, false, &sender);
                }
            }
            Msg::LayerProfile(which, i, p) => {
                if let Some(l) = self.layers_mut(which).get_mut(i) {
                    match &mut l.kind {
                        LabKind::Convert(to) => *to = p,
                        _ => l.input = p,
                    }
                    self.layers_changed(which, false, &sender);
                }
            }
            // Only the summary depends on these; the rows stay as they are.
            Msg::LabInput(p) => {
                self.lab_input = p;
                self.ui.lab.refresh_summary(&self.lab_layers, self.lab_blend, self.lab_input);
            }
            Msg::LookBlend(b) => {
                self.look_blend = b;
                self.show_looks();
                self.refresh_gallery();
            }
            Msg::LayerOn(which, i, on) => {
                if let Some(l) = self.layers_mut(which).get_mut(i) {
                    l.on = on;
                    self.layers_changed(which, false, &sender);
                }
            }
            Msg::FolderChanged => {
                self.folder_changes += 1;
                let (s, n) = (sender.input_sender().clone(), self.folder_changes);
                glib::timeout_add_local_once(std::time::Duration::from_millis(500), move || {
                    s.emit(Msg::FolderSettled(n))
                });
            }
            Msg::FolderSettled(n) if n == self.folder_changes => match settings::folder(settings::LUT_FOLDER) {
                Some(dir) => sender.input(Msg::Folder(dir)),
                None => self.toast(&gettext("The LUT folder was moved or deleted: press F5 to choose it again")),
            },
            Msg::FolderSettled(_) => {}
            Msg::LabBlend(b) => {
                self.lab_blend = b;
                self.ui.lab.refresh_summary(&self.lab_layers, self.lab_blend, self.lab_input);
                self.render_lab();
            }
            Msg::Page(page) => {
                self.page = page;
                // The header's Export LUT exports the grade; LUT Lab has its own export.
                self.ui.export.set_visible(matches!(page, Page::Grade));
                // Zoom is a Grade canvas view; the other pages show the whole photo.
                if !matches!(page, Page::Grade) {
                    self.set_zoom(None);
                }
                self.refresh();
            }
            Msg::Gpu(done) => self.on_gpu(done),
            Msg::Zoom(cmd) => self.on_zoom(cmd),
            Msg::CanvasSize(area) => {
                self.canvas = area;
                self.request_view();
                self.zoom_label();
            }
            Msg::CropEdit(on) if on == self.editing.is_some() => {}
            Msg::CropEdit(true) => {
                if self.decoded.is_none() || self.show_chart {
                    self.ui.crop.edit.set_active(false);
                    return;
                }
                self.set_zoom(None);
                self.editing = Some((self.crop, self.ratio, self.ui.crop.ratio.selected()));
                self.ui.crop.overlay.set_visible(true);
                self.crop_changed(&sender);
            }
            Msg::CropEdit(false) => {
                // A zoom while editing is into the whole frame, not the crop.
                self.set_zoom(None);
                self.editing = None;
                self.ui.crop.overlay.set_visible(false);
                self.ui.crop.edit.set_active(false);
                self.ui.crop.level.set_active(false);
                self.crop_changed(&sender);
            }
            Msg::CropCancel => {
                if let Some((crop, ratio, row)) = self.editing {
                    self.restore_crop(crop, ratio, row);
                    sender.input(Msg::CropEdit(false));
                }
            }
            Msg::CropRatio(i) => {
                let ratio = PRESETS.get(i).map_or(Ratio::Free, |p| p.1);
                self.ui.crop.swap.set_sensitive(matches!(ratio, Ratio::Fixed(a, b) if a != b));
                // Echo of a programmatic restore (Escape, reset): nothing to refit.
                if ratio != self.ratio {
                    self.ratio = ratio;
                    self.fit_crop();
                    self.crop_changed(&sender);
                }
            }
            Msg::CropSwap => {
                if let Ratio::Fixed(a, b) = self.ratio {
                    self.ratio = Ratio::Fixed(b, a);
                    self.fit_crop();
                    self.crop_changed(&sender);
                }
            }
            Msg::CropAngle(a) if a == self.crop.angle => {}
            Msg::CropAngle(a) => {
                if let Some((w, h)) = self.size() {
                    self.crop = crop::straighten(self.crop, a, w, h);
                }
                self.crop_changed(&sender);
            }
            Msg::CropRect(rect) => {
                self.crop.rect = rect;
                self.crop_changed(&sender);
            }
            Msg::CropReset => {
                self.restore_crop(Crop::default(), Ratio::Free, 0);
                self.crop_changed(&sender);
            }
            Msg::ViewReady(generation, profile, photo) => {
                // Only onto the photo it was cut from, in the profile the shader decodes.
                let shown = self.photo.as_ref().map(|p| p.1);
                if generation == self.view_gen && self.view_zoom().is_some() && shown == Some(profile) {
                    self.send(Job::SetView(photo));
                    self.sent = None;
                    self.render();
                }
            }
        }
    }
}

impl App {
    /// The zoom in effect: none on the test chart (the saved one returns with the photo).
    fn view_zoom(&self) -> Option<Zoom> {
        self.zoom.filter(|_| !self.show_chart)
    }

    /// Full-resolution size of the open photo.
    fn size(&self) -> Option<(u32, u32)> {
        Some(self.decoded.as_ref()?.size())
    }

    /// The crop in full-resolution pixels of the open photo.
    fn crop_region(&self) -> Option<Region> {
        let (w, h) = self.size()?;
        Some(crop::region(&self.crop, w, h))
    }

    /// What the preview shows: the whole straightened frame while editing
    /// the crop, otherwise the crop.
    fn shown_region(&self) -> Option<Region> {
        let (w, h) = self.size()?;
        let c = if self.editing.is_some() { crop::frame(self.crop) } else { self.crop };
        Some(crop::region(&c, w, h))
    }

    /// Crop state and its controls, state first so the controls' echoes
    /// (CropRatio, CropAngle) find nothing to change.
    fn restore_crop(&mut self, crop: Crop, ratio: Ratio, row: u32) {
        self.crop = crop;
        self.ratio = ratio;
        self.ui.crop.ratio.set_selected(row);
        self.ui.crop.angle.adj.set_value(crop.angle as f64);
    }

    /// Back to the uncropped photo, with every crop control in its default state.
    fn reset_crop(&mut self) {
        self.editing = None;
        self.restore_crop(Crop::default(), Ratio::Free, 0);
        self.ui.crop.edit.set_active(false);
        self.ui.crop.overlay.set_visible(false);
    }

    /// Shows a crop change: the overlay, and the preview when what it shows moved.
    fn crop_changed(&mut self, sender: &ComponentSender<Self>) {
        let size = self.size();
        {
            let mut shown = self.ui.crop.shown.borrow_mut();
            shown.crop = self.crop;
            shown.frame = size.map_or([0.0; 2], |(w, h)| [w as f32, h as f32]);
            shown.aspect = size.and_then(|(w, h)| self.ratio.aspect(w, h));
        }
        self.ui.crop.overlay.queue_draw();
        if self.shown_region() != self.encoded {
            self.encode_photo(sender);
        }
    }

    /// Largest crop of the current ratio at the current angle.
    fn fit_crop(&mut self) {
        if let Some((w, h)) = self.size() {
            self.crop = crop::refit(self.crop, self.ratio, w, h);
        }
    }

    /// Re-renders the zoomed view (if any) at full resolution, off the main thread.
    fn request_view(&mut self) {
        let (Some(z), Some(d), Some(crop)) = (self.view_zoom(), self.decoded.clone(), self.shown_region()) else {
            return;
        };
        let (r, w, h) = z.view(&crop, self.canvas);
        self.view_gen += 1;
        let (s, generation, profile) = (self.input.clone(), self.view_gen, self.profile);
        if d.is_raw() {
            let ((width, height), develop) = (self.draft_size((w, h)), self.develop);
            return self.send(Job::DevelopRaw { generation, region: r, width, height, develop, profile, view: true });
        }
        // shortcut: one thread per request; a scroll burst resamples views nobody sees. Pool them if it shows.
        let dev = Develop::default();
        std::thread::spawn(move || {
            s.emit(Msg::ViewReady(generation, profile, d.encode_region(&r, w, h, profile, &dev)))
        });
    }

    fn set_zoom(&mut self, zoom: Option<Zoom>) {
        let was = std::mem::replace(&mut self.zoom, zoom);
        self.ui.crop.shown.borrow_mut().zoomed = zoom.is_some();
        self.ui.crop.overlay.queue_draw();
        if zoom.is_some() {
            self.request_view();
        } else if was.is_some() {
            // Back to the fitted photo.
            self.view_gen += 1;
            if let Some((img, _)) = self.active().cloned() {
                self.send(Job::SetPhoto(img));
                self.sent = None;
                self.render();
            }
        }
        self.zoom_label();
    }

    fn zoom_label(&self) {
        let scale = match (self.zoom, self.shown_region()) {
            (Some(z), _) => z.scale,
            (None, Some(r)) => Zoom::fit_scale(&r, self.canvas),
            (None, None) => 1.0,
        };
        self.ui.zoom.label.set_label(&format!("{:.0}%", 100.0 / scale));
    }

    fn on_zoom(&mut self, cmd: ZoomCmd) {
        let zoomable = !self.show_chart && matches!(self.page, Page::Grade);
        let Some(crop) = self.shown_region().filter(|_| zoomable) else {
            return;
        };
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

    fn on_gpu(&mut self, done: Done) {
        match done {
            Done::Failed(e) => self.toast(&gettext!("GPU unavailable: {}", e)),
            Done::Graded { width, height, rgba, scopes: Scopes { histogram, waveform, vectorscope } } => {
                self.ui.grade_pic.set_paintable(Some(&texture(width, height, rgba)));
                let [h, w, v] = &self.ui.sidebar.scopes;
                h.set_paintable(Some(&texture(HIST_W as u32, HIST_H as u32, histogram)));
                w.set_paintable(Some(&texture(HIST_W as u32, HIST_H as u32, waveform)));
                v.set_paintable(Some(&texture(VEC_SIZE as u32, VEC_SIZE as u32, vectorscope)));
            }
            Done::Lab { width, height, rgba } => self.ui.lab.pic.set_paintable(Some(&texture(width, height, rgba))),
            Done::Thumb { generation, index, width, height, rgba } => {
                if generation != self.gallery_gen {
                    return;
                }
                if let Some(p) = self.ui.thumbs.get(index) {
                    p.set_paintable(Some(&texture(width, height, rgba)));
                }
            }
            Done::Exported(Ok(p)) => {
                self.toast(&gettext!("LUT saved: {}", p.file_name().unwrap_or_default().to_string_lossy()))
            }
            Done::Exported(Err(e)) => self.toast(&gettext!("Export failed: {}", e)),
            Done::RollFrame { generation, index, width, height, rgba } => {
                if generation == self.roll_gen {
                    self.ui.filmstrip.set_frame(index, &texture(width, height, rgba));
                }
            }
            Done::RawLoaded(generation, path, r) => {
                // The strip copy always; `Loaded` drops itself when a newer open superseded it.
                let small = r.as_ref().map(|d| roll_copy(d, self.profile)).map_err(String::clone);
                self.input.emit(Msg::RollLoaded(path.clone(), small));
                self.input.emit(Msg::Loaded(generation, path, r));
            }
            Done::Developed { view: false, profile, photo: Some(photo), .. } => {
                self.input.emit(Msg::Encoded(profile, photo))
            }
            Done::Developed { view: false, .. } => {}
            Done::Developed { view: true, generation, profile, .. } => {
                // Already what the GPU renders: only redraw, onto the photo it was cut from.
                let shown = self.photo.as_ref().map(|p| p.1);
                if generation == self.view_gen && self.view_zoom().is_some() && shown == Some(profile) {
                    self.sent = None;
                    self.render();
                }
            }
            Done::Sampled(c) => match self.camera().and_then(|cam| cam.neutral_for(c)) {
                Some(kt) => self.input.emit(Msg::Develop(DevelopEdit::Picked(kt))),
                None => self.toast(&gettext("Pick a grey area that is not too dark or too bright")),
            },
            Done::Matched(generation, _) if generation != self.match_gen => {}
            Done::Matched(_, m) => {
                if let Some(path) = self.camera_job.take() {
                    self.camera_match = Some((path, m.clone()));
                }
                let parts = std::mem::replace(&mut self.match_parts, (true, true));
                self.apply_match(&m, parts);
            }
        }
    }
}

fn profile_hint(p: Profile) -> String {
    if p.is_log() {
        gettext!("{} LUT → Rec.709. RAWs are encoded into this log; a JPEG/TIFF must already be in log.", p.name())
    } else {
        gettext("Signal the LUT will be applied to (Photo Style Standard, Vivid…).")
    }
}

/// Menu of export formats for action `action` (`win.<action>::<kind>-<size>`).
fn export_menu(action: &str, with_identity: bool) -> gio::Menu {
    let item =
        |m: &gio::Menu, label: &str, target: &str| m.append(Some(label), Some(&format!("win.{action}::{target}")));
    let menu = gio::Menu::new();
    // .cube is the universal 3D LUT format; the sizes differ only in precision.
    let cube = gio::Menu::new();
    item(&cube, &gettext("33³ .cube · cameras and monitors"), "cube-33");
    item(&cube, &gettext("65³ .cube · grading and editing"), "cube-65");
    item(&cube, &gettext("129³ .cube · maximum precision"), "cube-129");
    menu.append_section(Some(".cube"), &cube);
    let other = gio::Menu::new();
    item(&other, &gettext("Panasonic VLT · 17³ .vlt"), "vlt-17");
    item(&other, &gettext("HALD CLUT · 64³ .png"), "hald-64");
    menu.append_section(Some(gettext("Other formats").as_str()), &other);
    if with_identity {
        let tools = gio::Menu::new();
        item(&tools, &gettext("HALD identity · .png"), "identity-64");
        menu.append_section(Some(gettext("Tools").as_str()), &tools);
    }
    menu
}

/// Mixer coefficient `i`: 0..9 the RGB rows, 9..12 the black & white filter.
fn mixer_value_mut(p: &mut Params, i: usize) -> &mut f32 {
    if i < 9 { &mut p.mixer[i] } else { &mut p.mono_mix[i - 9] }
}

fn mixer_value(p: &Params, i: usize) -> f32 {
    *mixer_value_mut(&mut p.clone(), i)
}

fn wheel_mut(p: &mut Params, i: usize) -> &mut [f32; 4] {
    match i {
        0 => &mut p.lift,
        1 => &mut p.gamma,
        2 => &mut p.gain,
        _ => &mut p.offset,
    }
}

fn shortcuts_dialog() -> adw::ShortcutsDialog {
    let dialog = adw::ShortcutsDialog::new();
    let section = adw::ShortcutsSection::new(None);
    for (title, accel) in [
        (gettext("Open Photo"), "<Control>o"),
        (gettext("Export LUT"), "<Control>e"),
        (gettext("Before / after"), "backslash"),
        (gettext("Test chart"), "t"),
        (gettext("Show clipping"), "c"),
        (gettext("Skin tone line"), "k"),
        (gettext("Grid of all photos"), "g"),
        (gettext("Edit crop"), "r"),
        (gettext("Cancel crop"), "Escape"),
        (gettext("Apply crop"), "Return"),
        (gettext("Zoom to fit"), "<Control>0"),
        (gettext("Actual size"), "<Control>1"),
        (gettext("Zoom in"), "<Control>plus"),
        (gettext("Zoom out"), "<Control>minus"),
        (gettext("Save preset"), "<Control>s"),
        (gettext("Grade view"), "<Alt>1"),
        (gettext("LUT Lab view"), "<Alt>2"),
        (gettext("Browse LUTs"), "b"),
        (gettext("Search LUTs"), "<Control>f"),
        (gettext("Reload LUTs"), "F5"),
        (gettext("Preferences"), "<Control>comma"),
        (gettext("Keyboard Shortcuts"), "<Control>question"),
    ] {
        section.add(adw::ShortcutsItem::new(&title, accel));
    }
    dialog.add(section);
    dialog
}

fn fill(template: &str, args: &[String]) -> String {
    let mut parts = template.split("{}");
    let mut out = parts.next().unwrap_or_default().to_string();
    for (part, arg) in parts.zip(args.iter().map(String::as_str).chain(std::iter::repeat(""))) {
        out += arg;
        out += part;
    }
    out
}

/// Installed: `<prefix>/bin/rawvalley` → `<prefix>/share/locale`; dev: `make` puts catalogs in `target/locale`.
fn locale_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.parent()?.join("share/locale")))
        .filter(|d| d.join("it/LC_MESSAGES/rawvalley.mo").exists())
        .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../target/locale")))
}

/// What the thumbnail base is built from; look LUTs compare by identity, not content.
struct BaseKey {
    grade: Grade,
    luts: Vec<(Arc<Lut3d>, f32, Profile)>,
    input: Profile,
    blend: Blend,
}

impl PartialEq for BaseKey {
    fn eq(&self, o: &Self) -> bool {
        let same = |a: &(Arc<Lut3d>, f32, Profile), b: &(Arc<Lut3d>, f32, Profile)| {
            Arc::ptr_eq(&a.0, &b.0) && a.1 == b.1 && a.2 == b.2
        };
        self.grade == o.grade
            && self.input == o.input
            && self.blend == o.blend
            && self.luts.len() == o.luts.len()
            && self.luts.iter().zip(&o.luts).all(|(a, b)| same(a, b))
    }
}

thread_local! {
    /// The window's input, for files opened through the application (see `main`).
    static OPEN: std::cell::OnceCell<relm4::Sender<Msg>> = const { std::cell::OnceCell::new() };
}

fn main() {
    // gtk_init calls setlocale; a missing catalog just leaves the English source strings.
    let _ = gettextrs::bindtextdomain("rawvalley", locale_dir());
    let _ = gettextrs::bind_textdomain_codeset("rawvalley", "UTF-8");
    let _ = gettextrs::textdomain("rawvalley");
    // HANDLES_OPEN: files from the command line or a file manager ("Open With")
    // arrive here, also when RawValley is already running.
    let app = adw::Application::builder().application_id(APP_ID).flags(gio::ApplicationFlags::HANDLES_OPEN).build();
    app.connect_open(|app, files, _| {
        let paths: Vec<PathBuf> = files.iter().filter_map(|f| f.path()).collect();
        if !paths.is_empty() {
            OPEN.with(|o| o.get().map(|s| s.emit(Msg::RollAddPaths(paths))));
        }
        app.activate();
    });
    // When RawValley is already running this process only forwards the files,
    // and RelmApp::run then blocks on one last main-loop iteration: keep waking
    // the loop until we turn out to be the primary instance (startup).
    static STARTED: AtomicBool = AtomicBool::new(false);
    app.connect_startup(|_| {
        STARTED.store(true, Ordering::Relaxed);
        let css = CSS.replace("CANVAS_MARGIN", &CANVAS_MARGIN.to_string());
        relm4::set_global_css(&format!("{css}{}", band_css()));
    });
    std::thread::spawn(|| {
        while !STARTED.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(100));
            glib::MainContext::default().wakeup();
        }
    });
    RelmApp::from_app(app).with_args(std::env::args().collect()).run::<App>(());
}

/// File dialog for photos and RAWs, starting where the last photo was opened.
fn photo_dialog(title: &str) -> gtk::FileDialog {
    // add_suffix matches case-insensitively.
    let pats: Vec<String> = RASTER_EXTS.iter().chain(RAW_EXTS).map(|s| s.to_string()).collect();
    let dialog = gtk::FileDialog::builder()
        .title(title)
        .filters(&file_filters(&gettext("Photos and RAWs"), &pats))
        .modal(true)
        .build();
    dialog.set_initial_folder(start_in(settings::PHOTO_FOLDER).as_ref());
    dialog
}

fn preset_dialog(title: &str) -> gtk::FileDialog {
    let filters = file_filters(&gettext("RawValley presets"), &[preset::EXT.into()]);
    gtk::FileDialog::builder().title(title).filters(&filters).modal(true).build()
}

fn no_photo_to_match() -> String {
    gettext("Open a photo first: the match is computed on it")
}

/// Display-encoded float pixels as RGBA8, for thumbnails.
fn rgba8(pixels: &[[f32; 4]]) -> Vec<u8> {
    pixels.iter().flat_map(|p| p.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)).collect()
}
