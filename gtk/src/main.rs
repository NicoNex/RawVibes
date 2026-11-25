//! RawVibes GTK4/libadwaita front-end over `rawvibes-core`.

/// `gettext` with `{}` placeholders filled in order (translations may not reorder them).
macro_rules! gettext {
    ($msgid:literal, $($arg:expr),+ $(,)?) => {
        {
            let args = [$($arg.to_string()),+];
            $crate::fill(&$crate::gettext($msgid), &args)
        }
    };
}

mod crop_ui;
mod display_color;
mod export_ui;
mod filmstrip;
mod lab_page;
mod lens_ui;
mod meta_ui;
mod presets_ui;
mod settings;
mod widgets;
mod zoom_ui;

/// Icons bundled from relm4-icons by build.rs. Use them by their "-symbolic" name: the bare
/// name can hit a coloured icon of the same name in another theme (e.g. "color").
#[allow(dead_code)]
mod icon_names {
    include!(concat!(env!("OUT_DIR"), "/icon_names.rs"));
}

use std::cell::Cell;
use std::ffi::{CStr, CString};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};

use crop_ui::CropUi;
use export_ui::ExportUi;
use filmstrip::{Filmstrip, MAX_ROLL, ROLL_PX};
use lab_page::{LabAdd, LabKind, LabLayer, LabUi, Layers, caption};
use meta_ui::{MetaField, MetaUi};
use presets_ui::PresetsUi;
use rawvibes_core::chart::test_chart;
use rawvibes_core::crop::{self, Crop, PRESETS, Ratio};
use rawvibes_core::develop::{Camera, Detail, DevPart, Develop, KELVIN, WB_PRESETS};
use rawvibes_core::export::{self, PhotoFormat};
use rawvibes_core::gpu::{self, Done, Job, PhotoExport, ThumbBase};
use rawvibes_core::grade::{
    BAND_HUE_RANGE, BAND_HUES, Curve, Grade, HUE_HUE, HUE_SAT, LUM_SAT, LUMA, MASTER, Params, Stage, neutral_curve,
};
use rawvibes_core::lab::{Blend, Layer, Source, Stack, oklch_to_srgb};
use rawvibes_core::lut::{self, Format, Lut3d, LutEntry, MAX_LIBRARY};
use rawvibes_core::makernote::CameraLut;
use rawvibes_core::matching::{self, Match};
use rawvibes_core::meta::{self, RawMetadata};
use rawvibes_core::photo::{Decoded, PREVIEW_MAX, Photo, RASTER_EXTS, RAW_EXTS, is_photo, is_raw};
use rawvibes_core::preset::{self, Preset};
use rawvibes_core::profiles::Profile;
use rawvibes_core::raw::{RawData, read_raw};
use rawvibes_core::scopes::{HIST_H, HIST_W, Scopes, VEC_SIZE};
use rawvibes_core::sidecar::{Label, LensChoice, LookRef, Meta, Off, Sidecar};
use rawvibes_core::view::{Region, Zoom};
use rawvibes_core::{lens, lensdb};
use relm4::adw::prelude::*;
use relm4::gtk::{gdk, gio, glib};
use relm4::{ComponentParts, ComponentSender, RelmApp, SimpleComponent, adw, gtk};
use widgets::{ColorWheel, CurveEditor, Slider};
use zoom_ui::{ZoomCmd, ZoomUi};

const APP_ID: &str = "io.github.rawvibes.RawVibes";

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
    /// "As in camera": the camera JPEG (to cache) and the tone curve matched to it.
    CameraCurve(PathBuf, Option<Arc<Photo>>, Option<Curve>),
    /// A Real Time LUT the camera applied to the photo, and its file if found.
    CameraLut(PathBuf, CameraLut, Option<PathBuf>),
    /// Choose the file of photo `PathBuf`'s camera LUT that was not found.
    CameraLutFind(PathBuf, CameraLut),
    /// Use a camera LUT file (photo, LUT, file) in place of the matched curve.
    CameraLutAdd(PathBuf, CameraLut, PathBuf),
    AsInCameraOnOpen(bool),
    /// Write the sidecar if tick `u64` is still the last edit.
    SaveSidecar(u64),
    /// Same, without adding an undo step.
    WriteSidecar(u64),
    /// The window is closing: write pending edits, then close.
    Close,
    /// Look LUTs named by a sidecar, read.
    /// A sidecar's LUTs, loaded for `looks_gen`.
    SidecarLooks(u64, Vec<(LookRef, Result<Arc<Lut3d>, String>)>),
    /// A click on the canvas, in canvas device pixels.
    CanvasClick([f32; 2]),
    /// No development change for `DEVELOP_SETTLE` since tick `u64`: full quality.
    DevelopSettled(u64),
    /// The AI denoised the open RAW: develop again to show it.
    AiReady,
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
    /// A sidebar section switched off/on with its eye toggle.
    Off(Off, bool),
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
    /// A LUT file read for `Layers`, with its input and intensity when known.
    LayerLoaded(Layers, PathBuf, Option<(Profile, f32)>, Result<Lut3d, String>),
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
    /// Opens the Export Photo dialog, on this format if given.
    ExportPhoto(Option<PhotoFormat>),
    /// The dialog's format or quality changed.
    ExportSettings,
    /// The preview shows the HDR rendition.
    HdrPreview(bool),
    /// HDR headroom in stops above SDR white, and saving it once it rests.
    Headroom(f32),
    HeadroomSave(f32),
    /// Encodes the JPEG size estimate of this generation (for `w` × `h`), if still current.
    ExportEstimateRun(u64, u32, u32),
    /// A JPEG size estimate (estimate generation, bytes).
    ExportEstimate(u64, u64),
    /// The dialog's button: choose the file, or stop the running export.
    ExportGo,
    ExportPhotoTo(PathBuf),
    ExportClosed,
    /// Undo of a preset load: the grade before it.
    RestoreGrade(Box<Grade>),
    /// Undo of Reset all: every edit of the photo before it.
    /// Edits and the full look stack (path-less layers are not in the sidecar).
    RestoreEdits(Box<(Sidecar, Vec<LabLayer>)>),
    /// A Metadata tab field typed into.
    MetaEdit(MetaField, String),
    /// The camera's metadata of the photo loading as `load_gen`.
    SourceMeta(u64, PathBuf, Option<Arc<RawMetadata>>),
    /// The author defaults changed in Preferences.
    MetaDefaults,
    /// Lens profile row (lens_ui.rs): search text, a pick (None automatic, empty names no
    /// profile), and typed focal length / aperture.
    LensSearch(String),
    LensPick(Option<(String, String)>),
    LensManual(f32, f32),
    /// The lensfun database, read at start (`None`: not installed).
    LensDb(Option<Arc<lensdb::Db>>),
    /// Preferences: render the full-quality frame once development controls rest.
    Refine(bool),
    /// Filmstrip: add photos, a photo decoded small (already encoded for
    /// `Profile`), re-encoded for a new profile, pick or drop one, grid view.
    /// Photos for the filmstrip, and the one of them to open (else the first new one).
    RollAddPaths(Vec<PathBuf>, Option<PathBuf>),
    /// Merge to HDR: pick the bracketed RAWs, merge them, then the DNG written (or why not).
    MergeHdr,
    MergeHdrRun(Vec<PathBuf>),
    MergeHdrDone(Result<PathBuf, String>),
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
    /// Steps back or forward through the open photo's edits (Ctrl+Z, Ctrl+Shift+Z).
    Undo,
    Redo,
    /// Stars of the open photo; `true` clears them when they are already set (a star clicked twice).
    Rate(u8, bool),
    Mark(Label),
    /// Copies the open photo's edits; pastes them onto it, or onto every filmstrip photo.
    CopyEdits,
    PasteEdits,
    PasteEditsAll,
    /// The filmstrip filter changed.
    RollFilter,
    /// Exports every photo the filmstrip shows into this folder.
    BatchTo(PathBuf),
    Fullscreen,
}

/// A photo's edits and its look layers (those without a file too).
type Edits = (Sidecar, Vec<LabLayer>);

/// Most undo steps kept per photo.
const HISTORY_MAX: usize = 100;

/// A batch export in progress: photos still to export (last first), where to, and how it went.
/// What `find_lens` found for the open RAW, and what the Lens section shows of it.
struct LensView {
    profile: Option<lens::Profile>,
    /// The database lens matched or picked, as (maker, model).
    lens: Option<(String, String)>,
    status: lens_ui::Status,
    /// The focal length and aperture the profile is read at, and whether the rows ask for them.
    focal: f32,
    aperture: f32,
    asked: bool,
}

struct Batch {
    left: Vec<PathBuf>,
    dir: PathBuf,
    total: usize,
    failed: Vec<String>,
    /// The photo open before the batch, reopened at the end.
    back: Option<PathBuf>,
    /// Waiting for this photo to load before exporting it.
    waiting: Option<PathBuf>,
    /// Stop pressed: ends once the photo exporting now is done.
    stop: bool,
}

/// A photo of the filmstrip: its file, a small decoded copy once read, and
/// that copy encoded for the profile it was last shown in.
struct RollItem {
    path: PathBuf,
    /// The photo's own grade (its sidecar's), for its filmstrip frame.
    grade: Grade,
    /// Its look layers when last left this session: the sidecar keeps only those with a file.
    looks: Vec<LabLayer>,
    rating: u8,
    label: Label,
    small: Option<Decoded>,
    encoded: Option<(Profile, Arc<Photo>)>,
}

/// What an "As in camera" button sets.
#[derive(Debug, Clone, Copy)]
enum CameraPart {
    /// Reference match: the whole match to the camera JPEG.
    Match,
    /// The photo as the camera shot it: as-shot white balance and exposure
    /// from the RAW metadata, then RawTherapee's auto-matched tone curve
    /// (luminance only) to the camera JPEG in the master curve.
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
    /// A detail tool's slider.
    Detail(DetailField, f32),
    /// The chromatic-aberration switch.
    CaCorrect(bool),
    /// The lens profile's lateral CA correction.
    LensTca(bool),
    /// Noise reduction method: AI (true) or classic.
    NrAi(bool),
    Pick(bool),
    Picked([f32; 2]),
}

/// One of the detail tools' amounts.
type DetailField = fn(&mut Detail) -> &mut f32;

/// How the RAW preview trades resolution for fluidity while a development
/// control moves; the full-quality frame follows `DEVELOP_SETTLE` later.
#[derive(Clone, Copy, Debug, PartialEq)]
enum PreviewQuality {
    /// The draft scale that keeps a frame within `FRAME_BUDGET` on this GPU.
    Auto,
    Fast,
    Balanced,
    Best,
}

impl PreviewQuality {
    const ALL: [PreviewQuality; 4] =
        [PreviewQuality::Auto, PreviewQuality::Fast, PreviewQuality::Balanced, PreviewQuality::Best];

    fn key(self) -> &'static str {
        match self {
            PreviewQuality::Auto => "auto",
            PreviewQuality::Fast => "fast",
            PreviewQuality::Balanced => "balanced",
            PreviewQuality::Best => "best",
        }
    }

    fn label(self) -> String {
        match self {
            PreviewQuality::Auto => gettext("Automatic"),
            PreviewQuality::Fast => gettext("Fast"),
            PreviewQuality::Balanced => gettext("Balanced"),
            PreviewQuality::Best => gettext("Best"),
        }
    }

    /// Linear scale of the preview while dragging (`auto`: the measured one).
    fn draft(self, auto: f32) -> f32 {
        match self {
            PreviewQuality::Auto => auto,
            PreviewQuality::Fast => 0.5,
            PreviewQuality::Balanced => 0.75,
            PreviewQuality::Best => 1.0,
        }
    }

    fn stored() -> Self {
        let v = settings::value(settings::PREVIEW_QUALITY);
        Self::ALL.into_iter().find(|q| v.as_deref() == Some(q.key())).unwrap_or(PreviewQuality::Auto)
    }
}

/// Quiet time after the last edit before the sidecar is written.
const SAVE_SETTLE: std::time::Duration = std::time::Duration::from_millis(1000);
/// Edits reach the sidecar this long after the last one, so a crash loses next to nothing.
const WRITE_SETTLE: std::time::Duration = std::time::Duration::from_millis(250);

/// GPU time a draft frame may take under Automatic preview quality: a 60 Hz
/// frame, less what the grade and the redraw need.
const FRAME_BUDGET: f32 = 0.012;

/// The linear draft scale whose frame fits `FRAME_BUDGET` at `cost` GPU
/// seconds per pixel, for a full frame of `pixels`; never below 0.4 (softer
/// than that hides what the slider does).
fn auto_draft(cost: f32, pixels: u32) -> f32 {
    (FRAME_BUDGET / (cost * pixels as f32)).sqrt().clamp(0.4, 1.0)
}

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
    export_photo: ExportUi,
    chart_btn: gtk::ToggleButton,
    /// Header "As in camera" (RAWs only).
    camera_btn: gtk::Button,
    /// Header: the open photo's name, undo and redo, paste; under the photo, its stars and colour
    /// label and what took it.
    title: adw::WindowTitle,
    undo: gtk::Button,
    redo: gtk::Button,
    stars: Vec<gtk::Button>,
    label_btn: gtk::MenuButton,
    photo_info: gtk::Label,
    paste: gtk::Button,
    paste_all: gtk::Button,
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
    /// Sorted, so a section switched off and on again reads back the same.
    off: Vec<Off>,
    /// The open photo's own metadata (title, keywords, rights…).
    meta: Meta,
    /// The lens profile picked by hand for the open photo (`None`: from its EXIF).
    lens_profile: Option<LensChoice>,
    /// The lensfun database (`None` until read, or not installed).
    lens_db: Option<Arc<lensdb::Db>>,
    /// What a photo's camera recorded, and which photo: it arrives before that photo is open.
    source_meta: Option<(PathBuf, Arc<RawMetadata>)>,
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
    gpu_thread: Option<std::thread::JoinHandle<()>>,
    /// The running photo export's stop flag.
    exporting: Option<Arc<AtomicBool>>,
    /// The frame the JPEG size estimate encodes (`Done::ExportFrame`), and its generation.
    export_frame: Option<Arc<export::Exported>>,
    export_gen: u64,
    /// The preview shows the HDR rendition, `headroom` stops above SDR white (also the HDR exports').
    hdr_preview: bool,
    headroom: f32,
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
    /// Measured GPU seconds per pixel of a fitted develop (0: not yet), and
    /// the draft scale Automatic quality derives from it.
    frame_cost: f32,
    auto_draft: f32,
    /// Render the full-quality frame once development controls rest.
    refine: bool,
    develop_tick: u64,
    /// The camera match of a file, once computed.
    camera_match: Option<(PathBuf, Match)>,
    /// The last tone curve "As in camera" matched, and its photo, until the camera's LUT replaces it.
    camera_curve: Option<(PathBuf, Curve)>,
    /// The camera JPEG of a RAW, once read (As in camera and the camera match).
    camera_jpeg: Option<(PathBuf, Arc<Photo>)>,
    /// The match in flight is the camera's, for this file.
    camera_job: Option<PathBuf>,
    /// The open photo's edits as last read from or written to its sidecar.
    saved: Option<Sidecar>,
    save_tick: u64,
    /// Sidecar looks still loading: nothing is saved meanwhile.
    looks_loading: bool,
    /// Bumped whenever the looks are replaced: a sidecar load started before is stale.
    looks_gen: u64,
    /// A sidecar write failed this session (reported once).
    save_failed: bool,
    encode_again: bool,
    encoded: Option<Region>,
    /// The open photo's stars and colour label.
    rating: u8,
    label: Label,
    /// Undo history of the open photo's edits; `undo_at` is the step shown.
    history: Vec<Edits>,
    undo_at: usize,
    /// Edits copied with Copy edits.
    clipboard: Option<Edits>,
    batch: Option<Batch>,
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
        bypass_off(&mut g, &self.off);
        g
    }

    fn render(&mut self) {
        let mut grade = self.graded();
        grade.params.overlay = self.overlay as u32;
        if self.hdr_preview {
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
    fn sidecar_state(&self) -> Sidecar {
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
        let (rating, label) = (self.rating, self.label);
        let (profile, off, meta) = (Some(self.profile), self.off.clone(), self.meta.clone());
        // The lens profile is the photo's, found again on load: not an edit, nor saved.
        let develop = Develop { detail: Detail { lens: None, ..self.develop.detail }, ..self.develop };
        Sidecar {
            develop,
            grade,
            crop: self.crop,
            ratio: self.ratio,
            looks,
            look_blend,
            rating,
            label,
            profile,
            off,
            meta,
            lens_profile: self.lens_profile.clone(),
        }
    }

    /// Adds the open photo's edits to its undo history when they changed since the step shown.
    /// Stars, labels and metadata are not edits: they never make a step.
    fn record_edits(&mut self) {
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
        self.sync_undo();
    }

    /// A fresh history for the photo just opened, starting from its edits as read.
    fn reset_history(&mut self) {
        (self.history, self.undo_at) = (Vec::new(), 0);
        self.record_edits();
        self.sync_undo();
    }

    fn sync_undo(&self) {
        self.ui.undo.set_sensitive(self.undo_at > 0);
        self.ui.redo.set_sensitive(self.undo_at + 1 < self.history.len());
    }

    /// Shows undo step `back` (true) or forward, keeping the photo's stars, label and metadata.
    fn step_history(&mut self, back: bool, sender: &ComponentSender<Self>) {
        self.record_edits(); // an edit made under a second ago is a step too
        let to = if back { self.undo_at.checked_sub(1) } else { Some(self.undo_at + 1) };
        let Some((i, (mut sc, looks))) = to.and_then(|i| Some((i, self.history.get(i)?.clone()))) else { return };
        self.undo_at = i;
        (sc.rating, sc.label) = (self.rating, self.label);
        self.restore_edits((sc, looks), sender);
        self.sync_undo();
    }

    /// Puts `edits` on the open photo: develop, grade, crop and looks.
    fn restore_edits(&mut self, (mut sc, looks): Edits, sender: &ComponentSender<Self>) {
        let develop = self.develop;
        // Metadata is not an edit: undo, paste and undoing a reset leave what was typed.
        sc.meta = self.meta.clone();
        // Only onto a cleared photo, so its grade and looks come back too.
        self.apply_grade(Grade::default());
        sc.looks.clear();
        self.restore_sidecar(sc, sender);
        self.replace_looks(looks, sender);
        if self.develop != develop {
            self.develop_changed(sender);
        }
        self.crop_changed(sender);
    }

    /// The stars and label of the open photo in the header, and of every photo in the filmstrip.
    fn show_marks(&mut self) {
        let (rating, label) = (self.rating, self.label);
        // By path: while another photo loads, `active` already points at it.
        if let Some(it) = self.roll.iter_mut().find(|it| Some(&it.path) == self.path.as_ref()) {
            (it.rating, it.label) = (rating, label);
        }
        for (i, b) in self.ui.stars.iter().enumerate() {
            b.set_icon_name(if i < rating as usize { "starred-symbolic" } else { "non-starred-symbolic" });
        }
        filmstrip::set_label_class(&self.ui.label_btn, label);
        self.ui.label_btn.set_tooltip_text(Some(&gettext!("Colour label: {}", filmstrip::label_name(label))));
        let marks: Vec<(u8, Label)> = self.roll.iter().map(|it| (it.rating, it.label)).collect();
        self.ui.filmstrip.set_marks(&marks);
    }

    /// Pastes the copied edits onto every photo the filmstrip shows but the open one,
    /// straight into their sidecars. Returns how many took them, and the failures.
    fn paste_all(&mut self, (edits, looks): &Edits) -> (usize, Vec<String>) {
        let (mut n, mut failed) = (0, Vec::new());
        for (i, it) in self.roll.iter_mut().enumerate() {
            if Some(i) == self.active || !self.ui.filmstrip.shows(it.rating, it.label) {
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

    /// Ends the batch early and reopens the photo open before it.
    fn batch_stop(&mut self, sender: &ComponentSender<Self>) {
        self.batch_end(&gettext("Export stopped"), sender);
    }

    /// Ends the batch with `toast`, and reopens the photo open before it.
    fn batch_end(&mut self, toast: &str, sender: &ComponentSender<Self>) {
        let Some(b) = self.batch.take() else { return };
        self.ui.export_photo.set_busy(false);
        self.toast(toast);
        if let Some(i) = b.back.and_then(|p| self.roll.iter().position(|it| it.path == p))
            && Some(i) != self.active
        {
            self.load_active(i, sender);
        }
    }

    /// Exports the next photo of the batch, or ends it.
    fn batch_next(&mut self, sender: &ComponentSender<Self>) {
        if self.batch.as_ref().is_some_and(|b| b.stop) {
            return self.batch_stop(sender);
        }
        let Some(b) = self.batch.as_mut() else { return };
        let Some(path) = b.left.pop() else {
            let done = b.total - b.failed.len();
            let toast = if b.failed.is_empty() {
                gettext!("{} photos exported to {}", done, b.dir.display())
            } else {
                gettext!("{} photos exported, {} failed: {}", done, b.failed.len(), b.failed.join("; "))
            };
            self.batch_end(&toast, sender);
            self.ui.export_photo.dialog.close();
            return;
        };
        let i = b.total - b.left.len();
        self.ui.export_photo.set_step(&gettext!("Photo {} of {}", i, b.total));
        b.waiting = Some(path.clone());
        match self.roll.iter().position(|it| it.path == path) {
            Some(i) if Some(i) != self.active || self.decoded.is_none() => self.load_active(i, sender),
            Some(_) => self.batch_ready(sender),
            None => {
                b.failed.push(gettext!("{}: no longer in the filmstrip", file_label(&path)));
                self.batch_next(sender);
            }
        }
    }

    /// Exports the batch's photo once it is open, its looks loaded.
    fn batch_ready(&mut self, sender: &ComponentSender<Self>) {
        let Some(b) = self.batch.as_mut() else { return };
        if b.waiting.is_none() || b.waiting != self.path || self.looks_loading || self.decoded.is_none() {
            return;
        }
        let path = b.waiting.take().expect("checked above");
        let ext = self.ui.export_photo.format().ext();
        let stem = stem_label(&path).unwrap_or_default();
        let suffix = export_suffix();
        // Never over an existing file: a RAW+JPEG pair, or a photo of the strip itself.
        let out = free_path(&b.dir, &stem, ext, &suffix);
        if !self.export_to(out) {
            self.batch.as_mut().expect("checked above").failed.push(gettext!("{}: cannot export", file_label(&path)));
            self.batch_next(sender);
        }
    }

    /// The Metadata tab shows the open photo's metadata; with no photo, empty and not editable.
    fn sync_meta(&self) {
        // Under the photo: camera · lens · focal length.
        let info = self.camera_meta().map(|m| {
            let focal = m.exif.focal_length.filter(|r| r.d != 0).map(|r| format!("{} mm", r.as_f32().round()));
            [Some(meta::camera_name(m)), meta::lens_name(m), focal]
                .into_iter()
                .flatten()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" · ")
        });
        self.ui.photo_info.set_label(&info.unwrap_or_default());
        let ui = &self.ui.sidebar.meta;
        ui.set(&self.meta, &author_defaults());
        let policy = export_policy();
        ui.show_policy(&policy_name(policy), &policy_hint(policy));
        ui.show_camera(self.camera_meta());
        ui.root.set_sensitive(self.path.is_some());
    }

    /// The open RAW's lens profile from the database: the one picked, or the one its EXIF names.
    fn find_lens(&self) -> LensView {
        use lens_ui::Status;
        let src = self.camera_meta();
        let exif = src.map(|m| &m.exif);
        let value = |r: Option<meta::Rational>| r.filter(|r| r.d != 0).map_or(0.0, |r| r.as_f32());
        let (exif_focal, aperture) = (value(exif.and_then(|e| e.focal_length)), value(exif.and_then(|e| e.fnumber)));
        let distance = value(exif.and_then(|e| e.subject_distance));
        let mut view = LensView {
            profile: None,
            lens: None,
            status: Status::NoDatabase,
            focal: exif_focal,
            aperture,
            asked: false,
        };
        let Some(db) = self.lens_db.as_deref() else { return view };
        let camera = src.and_then(|m| db.find_camera(&m.make, &m.model));
        let (lens, chosen) = match &self.lens_profile {
            Some(c) if c.model.is_empty() => return LensView { status: Status::Off, ..view },
            Some(c) => {
                let pick = |typed: f32, exif: f32| if typed > 0.0 { typed } else { exif };
                (view.focal, view.aperture) = (pick(c.focal, exif_focal), pick(c.aperture, aperture));
                (db.lens(&c.maker, &c.model), true)
            }
            None => (db.find_lens(camera, "", &src.and_then(meta::lens_name).unwrap_or_default()), false),
        };
        let Some(found) = lens else { return LensView { status: Status::NotFound, asked: chosen, ..view } };
        let name = found.model.clone();
        view.lens = Some((found.maker.clone(), name.clone()));
        match lens::calib_at(found, view.focal, view.aperture, distance) {
            Some(c) => LensView {
                profile: Some(lens::Profile::new(found, c, camera.map(|c| c.crop))),
                status: if chosen { Status::Chosen(name) } else { Status::Auto(name) },
                // A manual lens records no focal length: its row stays open to change it.
                asked: chosen || exif_focal <= 0.0,
                ..view
            },
            None => LensView { status: Status::NeedsFocal(name), asked: true, ..view },
        }
    }

    /// Puts the open RAW's lens profile into the development and the Lens section;
    /// re-develops when it changed and `redevelop` says so.
    fn update_lens(&mut self, sender: &ComponentSender<Self>, redevelop: bool) {
        let LensView { profile, status, focal, aperture, asked, .. } = self.find_lens();
        let changed = self.develop.detail.lens != profile;
        self.develop.detail.lens = profile;
        let ui = &self.ui.sidebar.develop;
        ui.lens_profile.show(&status, focal, aperture, asked);
        ui.lens_amounts.set_sensitive(profile.is_some());
        if changed && redevelop && self.camera().is_some() {
            self.develop_changed(sender);
        }
    }

    /// What the open photo's camera recorded; never another photo's, whose load is still on its way.
    fn camera_meta(&self) -> Option<&RawMetadata> {
        self.source_meta.as_ref().filter(|(p, _)| Some(p) == self.path.as_ref()).map(|(_, m)| &**m)
    }

    /// The metadata an export of `crop` embeds: the photo's, with the author defaults, as Preferences allow.
    fn export_tags(&self, crop: &Region) -> meta::Tags {
        let src = self.camera_meta();
        let m = meta::with_defaults(&self.meta, &author_defaults(), src.and_then(meta::capture_year));
        meta::tags(src, &m, export_policy(), Decoded::fit_size(crop, u32::MAX))
    }

    /// Starts exporting the open photo to `path`; false if there is none.
    fn export_to(&mut self, path: PathBuf) -> bool {
        let (Some(decoded), Some(crop)) = (self.decoded.clone(), self.crop_region()) else { return false };
        let cancel = Arc::new(AtomicBool::new(false));
        self.exporting = Some(cancel.clone());
        let ui = &self.ui.export_photo;
        if self.batch.is_none() {
            ui.set_busy(true);
        }
        // Remembered as used, not per spin tick.
        let _ = settings::set_value(settings::EXPORT_FORMAT, ui.format().tag());
        let _ = settings::set_value(settings::JPEG_QUALITY, &ui.quality().to_string());
        let (format, quality, headroom) = (ui.format(), ui.quality(), self.headroom);
        let tags = self.export_tags(&crop);
        if matches!(format, PhotoFormat::Jpeg | PhotoFormat::UltraHdr)
            && tags.xmp.as_deref().is_some_and(|x| !export::xmp_fits_jpeg(x))
        {
            self.toast(&gettext("Title, keywords and rights are too long for a JPEG: exported without them"));
        }
        self.send(Job::ExportPhoto(Box::new(PhotoExport {
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
        })));
        true
    }

    /// Replaces the look stack: a sidecar load still in flight becomes stale.
    /// Returns the stack it replaced.
    fn replace_looks(&mut self, looks: Vec<LabLayer>, sender: &ComponentSender<Self>) -> Vec<LabLayer> {
        self.looks_gen += 1;
        self.looks_loading = false;
        let old = std::mem::replace(&mut self.looks, looks);
        self.layers_changed(Layers::Look, true, sender);
        old
    }

    /// Writes the sidecar shortly after the last edit, and makes it an undo step a second after.
    fn schedule_save(&mut self) {
        if self.saved.is_none() {
            return;
        }
        self.save_tick += 1;
        let (s, tick) = (self.input.clone(), self.save_tick);
        let w = s.clone();
        glib::timeout_add_local_once(WRITE_SETTLE, move || w.emit(Msg::WriteSidecar(tick)));
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
        (self.rating, self.label, self.meta) = (sc.rating, sc.label, sc.meta);
        self.lens_profile = sc.lens_profile;
        self.set_offs(&sc.off);
        if let Some(p) = sc.profile {
            // Set now, so the state read back matches; the dropdown's Msg::Profile echo re-encodes.
            self.profile = p;
            self.ui.sidebar.profile_sel.set_selected(Profile::ALL.iter().position(|&q| q == p).unwrap_or(0) as u32);
        }
        self.develop = sc.develop;
        // The profile is the photo's, never the snapshot's (undo, paste): found again.
        self.update_lens(sender, false);
        self.sync_develop();
        let row = crop::PRESETS
            .iter()
            .position(|(_, r)| {
                *r == sc.ratio
                    || matches!((r, sc.ratio), (Ratio::Fixed(a, b), Ratio::Fixed(c, d)) if (*a, *b) == (d, c))
            })
            .unwrap_or(0);
        self.restore_crop(sc.crop, sc.ratio, row as u32);
        self.apply_grade(sc.grade);
        self.ui.sidebar.look_blend_toggle.set_active_name(Some(if sc.look_blend == Blend::Mix {
            "mix"
        } else {
            "chain"
        }));
        if !sc.looks.is_empty() {
            self.looks_loading = true;
            let (s, refs, generation) = (sender.input_sender().clone(), sc.looks, self.looks_gen);
            std::thread::spawn(move || {
                let loaded = refs.into_iter().map(|r| {
                    let lut = Lut3d::load(&r.path).map(Arc::new);
                    (r, lut)
                });
                s.emit(Msg::SidecarLooks(generation, loaded.collect()));
            });
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
        self.camera_job = None;
        self.match_gen += 1;
        self.ui.sidebar.matching.stack.set_visible_child_name("busy");
        self.send(Job::Match { generation: self.match_gen, source, reference, input: self.grade.neutralized() });
    }

    /// Rebuilds the filmstrip cards after photos were added or removed.
    fn roll_changed(&mut self, sender: &ComponentSender<Self>) {
        // Remembered for the next start; a failed write only loses the resume.
        let roll: Vec<String> = self.roll.iter().map(|it| it.path.to_string_lossy().into_owned()).collect();
        let _ = settings::set_value(settings::ROLL, &roll.join("\n"));
        let names: Vec<String> = self.roll.iter().map(|it| file_label(&it.path)).collect();
        self.ui.filmstrip.rebuild(&names, self.active, sender);
        self.show_marks();
        self.ui.paste_all.set_sensitive(self.clipboard.is_some() && self.roll.len() > 1);
        self.ui.grid_btn.set_sensitive(self.roll.len() > 1);
        if self.roll.len() < 2 && self.ui.grid_btn.is_active() {
            self.ui.grid_btn.set_active(false);
        }
        self.upload_roll(true);
    }

    /// Adds `paths` to the filmstrip and opens `open_at`, or else the first of them.
    fn add_to_roll(&mut self, paths: Vec<PathBuf>, open_at: Option<PathBuf>, sender: &ComponentSender<Self>) {
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
                    let sc = Sidecar::load(&p).ok().flatten().unwrap_or_default();
                    self.roll.push(RollItem {
                        path: p.clone(),
                        grade: card_grade(&sc),
                        rating: sc.rating,
                        label: sc.label,
                        looks: Vec::new(),
                        small: None,
                        encoded: None,
                    });
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
            self.roll_changed(sender);
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
            self.load_active(i, sender);
        }
    }

    /// Opens filmstrip photo `i` for editing (full-size decode off the main thread).
    fn load_active(&mut self, i: usize, sender: &ComponentSender<Self>) {
        let Some(path) = self.roll.get(i).map(|it| it.path.clone()) else { return };
        self.active = Some(i);
        self.ui.filmstrip.set_active(self.active);
        let _ = settings::set_value(settings::LAST_PHOTO, &path.to_string_lossy());
        // ponytail: a failed save only loses the dialog's start folder.
        if let Some(dir) = path.parent() {
            let _ = settings::set_folder(settings::PHOTO_FOLDER, dir);
        }
        self.ui.grade_stack.set_visible_child_name(if self.has_source() { "image" } else { "loading" });
        self.ui.info.set_label(&gettext("Loading…"));
        self.load_gen += 1;
        let (s, generation, profile) = (sender.input_sender().clone(), self.load_gen, self.profile);
        std::thread::spawn(move || {
            // Headers only, before queuing behind another decode.
            s.emit(Msg::SourceMeta(generation, path.clone(), meta::read(&path).map(Arc::new)));
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
        self.action_toast(title, &gettext("Undo"), undo);
    }

    /// Toast with a button `label` that sends `msg()`.
    fn action_toast(&self, title: &str, label: &str, msg: impl Fn() -> Msg + 'static) {
        let toast = adw::Toast::builder().title(title).button_label(label).timeout(6).build();
        let s = self.input.clone();
        toast.connect_button_clicked(move |_| s.emit(msg()));
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
        let s = sender.input_sender().clone();
        std::thread::spawn(move || {
            s.emit(Msg::Encoded(profile, d.encode_fit(&r, PREVIEW_MAX, profile, &Develop::default())))
        });
    }

    fn camera(&self) -> Option<&Camera> {
        self.decoded.as_ref()?.camera()
    }

    /// Switches the sections in `offs` off and the others on; the eyes' echoes re-render what changed.
    fn set_offs(&mut self, offs: &[Off]) {
        self.off = offs.to_vec();
        self.off.sort();
        EYES.with(|e| e.borrow().iter().for_each(|(p, eye)| eye.set_active(offs.contains(p))));
    }

    /// The development the preview and export use: `develop` without the sections switched off.
    fn applied_develop(&self) -> Develop {
        let mut d = self.develop;
        self.off.iter().for_each(|o| {
            if let Off::Develop(p) = o {
                p.neutral(&mut d)
            }
        });
        d
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
            DevelopEdit::Detail(field, v) => *field(&mut dev.detail) = v,
            DevelopEdit::CaCorrect(on) => dev.detail.ca_correct = on,
            DevelopEdit::LensTca(on) => dev.detail.lens_tca = on,
            DevelopEdit::NrAi(on) => dev.detail.nr_ai = on,
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
        let ui = &self.ui.sidebar.develop;
        // Always there, so the editor looks the same before a photo opens; only RAWs develop.
        for w in [&ui.root, &ui.lens, &ui.detail_root] {
            w.set_sensitive(raw);
            w.set_tooltip_text((!raw).then(|| gettext("RAW development: open a RAW file to use it")).as_deref());
        }
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
        let mut d = self.develop.detail;
        for (s, field) in &ui.detail {
            s.adj.set_value(*field(&mut d) as f64);
        }
        ui.nr_detail.root.set_sensitive(d.nr_luma > 0.0);
        ui.nr_mode.set_active_name(Some(if d.nr_ai { "ai" } else { "classic" }));
        ui.nr_strength.root.set_visible(d.nr_ai);
        ui.nr_luma.root.set_visible(!d.nr_ai);
        ui.nr_detail.root.set_visible(!d.nr_ai);
        if !d.nr_ai {
            ui.ai_busy.set_visible(false);
            ui.ai_msg.set_visible(false);
        }
        ui.ca.set_active(d.ca_correct);
        ui.lens_tca.set_active(d.lens_tca);
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
    fn apply_match(&mut self, m: &Match) {
        matching::apply(&mut self.grade, m);
        let ui = &self.ui.sidebar.matching;
        ui.amount.adj.set_value(100.0);
        ui.stack.set_visible_child_name("set");
        self.render();
    }

    /// The develop as the camera shot the photo: as-shot white balance and exposure.
    fn as_shot(&self) -> Option<Develop> {
        self.camera().map(|cam| Develop { wb: None, exposure: cam.baseline_ev, ..self.develop })
    }

    /// "As in camera": WB and exposure from the metadata plus the matched
    /// tone curve (`All`), or the full match to the camera JPEG (`Match`,
    /// computed once per file).
    fn as_in_camera(&mut self, part: CameraPart, sender: &ComponentSender<Self>) {
        let (Some(path), true) = (self.path.clone(), self.camera().is_some()) else { return };
        if matches!(part, CameraPart::All) {
            self.edit_develop(DevelopEdit::AsShot, sender);
            self.edit_develop(DevelopEdit::CameraExposure, sender);
            let (Some(d), Some(develop)) = (self.decoded.clone(), self.as_shot()) else { return };
            let library: Vec<PathBuf> = self.luts.iter().map(|l| l.path.clone()).collect();
            let cached = self.camera_jpeg.clone().filter(|c| c.0 == path).map(|c| c.1);
            let s = sender.input_sender().clone();
            std::thread::spawn(move || {
                let jpeg = cached.or_else(|| rawvibes_core::photo::embedded_preview(&path).ok().map(Arc::new));
                let curve = jpeg.as_ref().and_then(|jpeg| {
                    // The master curve works on display values: Standard is the display encoding whatever the input profile.
                    let neutral = as_shot_render(&d, &develop, Profile::Standard);
                    rawvibes_core::histmatch::auto_tone_curve(&neutral, jpeg, d.size().0)
                });
                s.emit(Msg::CameraCurve(path.clone(), jpeg, curve));
                // The Real Time LUTs are offered in place of the curve, not on top of it.
                for lut in rawvibes_core::makernote::real_time_luts(&path) {
                    let remembered = settings::value(&camera_lut_key(&lut.name)).map(PathBuf::from);
                    let file = remembered
                        .filter(|p| p.is_file())
                        .or_else(|| lut::find_named(library.iter().map(PathBuf::as_path), &lut.name));
                    s.emit(Msg::CameraLut(path.clone(), lut, file));
                }
            });
            return;
        }
        if let Some((_, m)) = self.camera_match.clone().filter(|c| c.0 == path) {
            return self.apply_match(&m);
        }
        if let Some((_, jpeg)) = self.camera_jpeg.clone().filter(|c| c.0 == path) {
            return sender.input(Msg::CameraJpeg(path, Ok(jpeg)));
        }
        self.ui.sidebar.matching.stack.set_visible_child_name("busy");
        let s = sender.input_sender().clone();
        std::thread::spawn(move || {
            let r = rawvibes_core::photo::embedded_preview(&path).map(Arc::new);
            s.emit(Msg::CameraJpeg(path, r));
        });
    }

    /// `size` at the draft scale while a development control moves.
    fn draft_size(&self, (w, h): (u32, u32)) -> (u32, u32) {
        let k = if self.drafting { self.quality.draft(self.auto_draft) } else { 1.0 };
        (((w as f32 * k) as u32).max(1), ((h as f32 * k) as u32).max(1))
    }

    /// Re-develops the RAW after a Develop change: a draft now, full quality once it settles.
    fn develop_changed(&mut self, sender: &ComponentSender<Self>) {
        self.drafting = self.quality.draft(self.auto_draft) < 1.0;
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
    fn load_layer(which: Layers, path: PathBuf, known: Option<(Profile, f32)>, sender: &ComponentSender<Self>) {
        let s = sender.input_sender().clone();
        std::thread::spawn(move || s.emit(Msg::LayerLoaded(which, path.clone(), known, Lut3d::load(&path))));
    }

    /// Adds `lut` to `which`, with its input and intensity if known.
    fn push_lut(
        &mut self,
        which: Layers,
        lut: Arc<Lut3d>,
        path: Option<PathBuf>,
        known: Option<(Profile, f32)>,
        sender: &ComponentSender<Self>,
    ) {
        // Vendors name the input in the file (e.g. "…_FLog_…"): preselect it.
        let (input, intensity) = known.unwrap_or_else(|| (Profile::guess_from_name(&lut.title), 1.0));
        self.layers_mut(which).push(LabLayer { kind: LabKind::Lut(lut), intensity, input, on: true, path });
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
                    self.ui.sidebar.applied_dock.set_visible(!self.looks.is_empty());
                    // Show where a LUT added from the browser went.
                    let last = self.ui.sidebar.looks.row_at_index(self.looks.len() as i32 - 1);
                    if let Some(row) = last.filter(|_| std::mem::take(&mut self.flash_new_look)) {
                        row.add_css_class("flash");
                        // Under the grid the list scrolls: bring the new LUT into view once it is laid out.
                        let adj = self.ui.sidebar.applied_dock.vadjustment();
                        glib::idle_add_local_once(move || adj.set_value(adj.upper() - adj.page_size()));
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
        if let Some((_, lut)) = self.candidate.as_ref().filter(|_| !self.off.contains(&Off::Looks)) {
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
        if self.off.contains(&Off::Looks) {
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

    /// Renders the frame the size estimate encodes (`Done::ExportFrame`): the photo
    /// graded at preview size, with its HDR gains when the format carries them.
    fn export_frame_request(&mut self) {
        self.export_frame = None;
        self.export_gen += 1;
        self.ui.export_photo.set_estimate(None);
        let ui = &self.ui.export_photo;
        let mut grade = self.graded();
        if ui.format().hdr() {
            grade.params.hdr_stops = self.headroom;
        }
        self.send(Job::ExportFrame(grade));
    }

    /// The export dialog's file size: the preview-size frame encoded as exported
    /// (off the main thread once the settings settle), scaled by the pixel count.
    fn export_estimate(&mut self, sender: &ComponentSender<Self>) {
        let Some((w, h)) = self.crop_region().map(|c| Decoded::fit_size(&c, u32::MAX)) else { return };
        self.export_gen += 1;
        if self.ui.export_photo.format() == PhotoFormat::Tiff16 {
            // Uncompressed: exact without encoding.
            return self.ui.export_photo.set_estimate(Some(glib::format_size(w as u64 * h as u64 * 6).into()));
        }
        self.ui.export_photo.set_estimate(None);
        let (generation, s) = (self.export_gen, sender.input_sender().clone());
        // A spin held down sends a value per tick: encode only the one it stops on.
        glib::timeout_add_local_once(std::time::Duration::from_millis(250), move || {
            s.emit(Msg::ExportEstimateRun(generation, w, h))
        });
    }

    fn toast(&self, text: &str) {
        self.ui.toasts.add_toast(adw::Toast::builder().title(text).timeout(4).build());
    }
}

/// An RGBA8 texture; photos arrive already in the monitor's colour space (`display_color`).
fn texture(width: u32, height: u32, rgba: Vec<u8>) -> gdk::MemoryTexture {
    let bytes = glib::Bytes::from_owned(rgba);
    gdk::MemoryTexture::new(width as i32, height as i32, gdk::MemoryFormat::R8g8b8a8, &bytes, width as usize * 4)
}

/// An RGBA16F texture in GTK's rec2100-linear (linear Rec.2020, 1 = SDR white): the
/// HDR preview. GTK hands it to the compositor as HDR where colour management is on.
fn hdr_texture(width: u32, height: u32, rgba16f: Vec<u8>) -> gdk::Texture {
    gdk::MemoryTextureBuilder::new()
        .set_width(width as i32)
        .set_height(height as i32)
        .set_format(gdk::MemoryFormat::R16g16b16a16Float)
        .set_bytes(Some(&glib::Bytes::from_owned(rgba16f)))
        .set_stride(width as usize * 8)
        .set_color_state(&gdk::ColorState::rec2100_linear())
        .build()
}

/// Whether the preview shows the HDR rendition: on Linux only (GTK's macOS backend has no HDR).
fn hdr_preview_setting() -> bool {
    cfg!(target_os = "linux") && settings::value(settings::HDR_PREVIEW).as_deref() == Some("1")
}

/// Name of an exported photo: never the original's own.
fn edit_name(stem: &str, ext: &str, suffix: &str) -> String {
    // A suffix is part of a file name: no folders, never empty (it would overwrite the photo's own JPEG).
    let suffix: String = suffix.trim().chars().filter(|c| !matches!(c, '/' | '\\')).collect();
    let suffix = if suffix.is_empty() { "_edit" } else { &suffix };
    format!("{stem}{suffix}.{ext}")
}

/// How much metadata exports carry, from Preferences.
fn export_policy() -> meta::Policy {
    settings::value(settings::METADATA_POLICY).and_then(|t| meta::Policy::from_tag(&t)).unwrap_or_default()
}

/// The export file name suffix from Preferences.
fn export_suffix() -> String {
    settings::value(settings::EXPORT_SUFFIX).unwrap_or_default()
}

/// The policy's name as the Preferences row lists it.
fn policy_name(p: meta::Policy) -> String {
    match p {
        meta::Policy::All => gettext("All"),
        meta::Policy::Essential => gettext("Essential"),
        meta::Policy::Copyright => gettext("Copyright only"),
        meta::Policy::None => gettext("None"),
    }
}

/// What each export policy leaves in a photo, for the Preferences row.
fn policy_hint(p: meta::Policy) -> String {
    match p {
        meta::Policy::All => gettext("Everything the camera recorded, location and serial numbers included"),
        meta::Policy::Essential => {
            gettext("Camera, exposure, date, rights and description; no location, no serial numbers")
        }
        meta::Policy::Copyright => gettext("Only the creator, copyright and contact"),
        meta::Policy::None => gettext("No metadata at all"),
    }
}

/// Preferences → Metadata: what exports carry, their file name, the author stamped on them.
fn metadata_prefs(sender: &ComponentSender<App>) -> adw::PreferencesPage {
    let labels = meta::Policy::ALL.map(policy_name);
    let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
    let stored = export_policy();
    let policy = adw::ComboRow::builder()
        .title(gettext("Metadata in exported photos"))
        .subtitle(policy_hint(stored))
        .model(&gtk::StringList::new(&labels))
        .selected(meta::Policy::ALL.iter().position(|&p| p == stored).unwrap_or(1) as u32)
        .build();
    let snd = sender.clone();
    policy.connect_selected_notify(move |r| {
        let p = meta::Policy::ALL.get(r.selected() as usize).copied().unwrap_or_default();
        let _ = settings::set_value(settings::METADATA_POLICY, p.tag());
        r.set_subtitle(&policy_hint(p));
        // The Metadata tab's note says what exports carry.
        snd.input(Msg::MetaDefaults);
    });
    let suffix = adw::EntryRow::builder().title(gettext("File name suffix")).text(export_suffix()).build();
    suffix.connect_changed(|r| {
        let _ = settings::set_value(settings::EXPORT_SUFFIX, &r.text());
    });
    let export = adw::PreferencesGroup::builder()
        .title(gettext("Export"))
        .description(gettext("Exported photos are named after the original plus this suffix (_edit when empty)."))
        .build();
    export.add(&policy);
    export.add(&suffix);
    let author = adw::PreferencesGroup::builder()
        .title(gettext("Author"))
        .description(gettext(
            "Written into every export whose photo leaves the field empty. In the copyright, {year} becomes the year the photo was taken and {creator} the creator.",
        ))
        .build();
    let d = author_defaults();
    for (key, title, text) in [
        (settings::AUTHOR, gettext("Creator"), d.creator),
        (settings::COPYRIGHT, gettext("Copyright"), d.copyright),
        (settings::AUTHOR_EMAIL, gettext("Email"), d.email),
        (settings::AUTHOR_WEBSITE, gettext("Website"), d.website),
    ] {
        let row = adw::EntryRow::builder().title(title).text(text).build();
        let snd = sender.clone();
        row.connect_changed(move |r| {
            let _ = settings::set_value(key, &r.text());
            snd.input(Msg::MetaDefaults);
        });
        author.add(&row);
    }
    let page = adw::PreferencesPage::builder().title(gettext("Metadata")).icon_name("tag-outline-symbolic").build();
    page.add(&export);
    page.add(&author);
    page
}

/// A sidebar width the editor keeps usable: sliders fit, the photo keeps most of the window.
fn clamp_sidebar(w: f64) -> f64 {
    w.clamp(300.0, 720.0)
}

/// The copyright Preferences start from.
const COPYRIGHT_TEMPLATE: &str = "© {year} {creator}";

/// The author fields from Preferences, which a photo's empty fields take.
fn author_defaults() -> Meta {
    let get = |k| settings::value(k).unwrap_or_default();
    Meta {
        creator: get(settings::AUTHOR),
        copyright: settings::value(settings::COPYRIGHT).unwrap_or_else(|| COPYRIGHT_TEMPLATE.into()),
        email: get(settings::AUTHOR_EMAIL),
        website: get(settings::AUTHOR_WEBSITE),
        ..Default::default()
    }
}

/// `clip`'s edits onto `target`, which keeps its crop, stars, label and metadata.
fn pasted(clip: &Sidecar, target: &Sidecar) -> Sidecar {
    // The input profile, like the crop (and its eye), belongs to the photo pasted onto.
    let mut off: Vec<Off> = clip.off.iter().copied().filter(|&o| o != Off::Crop).collect();
    if target.off.contains(&Off::Crop) {
        off.push(Off::Crop);
        off.sort();
    }
    Sidecar {
        crop: target.crop,
        ratio: target.ratio,
        rating: target.rating,
        label: target.label,
        profile: target.profile,
        off,
        meta: target.meta.clone(),
        // The lens, like the crop, belongs to the photo pasted onto.
        lens_profile: target.lens_profile.clone(),
        ..clip.clone()
    }
}

/// A filmstrip card's grade: the sidecar's, with its switched-off sections neutral.
fn card_grade(sc: &Sidecar) -> Grade {
    let mut g = sc.grade.clone();
    bypass_off(&mut g, &sc.off);
    g
}

/// Neutralises the grade stages in `off`.
fn bypass_off(g: &mut Grade, off: &[Off]) {
    off.iter().for_each(|o| {
        if let Off::Grade(s) = o {
            g.bypass(*s)
        }
    });
}

/// Header button sending `msg`.
fn header_button(icon: &str, tip: &str, msg: impl Fn() -> Msg + 'static, sender: &ComponentSender<App>) -> gtk::Button {
    let b = gtk::Button::builder().icon_name(icon).tooltip_text(tip).build();
    b.update_property(&[gtk::accessible::Property::Label(tip)]);
    let snd = sender.clone();
    b.connect_clicked(move |_| snd.input(msg()));
    b
}

/// Colour label picker: a dot that takes the label's colour, and a popover of the labels.
fn label_menu(sender: &ComponentSender<App>) -> gtk::MenuButton {
    let list = vbox(2);
    let popover = gtk::Popover::builder().child(&list).build();
    for l in Label::ALL {
        let row = gtk::Box::builder().spacing(8).build();
        let dot = gtk::Box::builder().css_classes(["label-dot", filmstrip::label_class(l)]).build();
        dot.set_valign(gtk::Align::Center);
        row.append(&dot);
        row.append(&gtk::Label::new(Some(&filmstrip::label_name(l))));
        let b = gtk::Button::builder().child(&row).css_classes(["flat"]).build();
        let (snd, p) = (sender.clone(), popover.clone());
        b.connect_clicked(move |_| {
            p.popdown();
            snd.input(Msg::Mark(l));
        });
        list.append(&b);
    }
    let dot =
        gtk::Box::builder().css_classes(["label-dot"]).valign(gtk::Align::Center).halign(gtk::Align::Center).build();
    let b = gtk::MenuButton::builder()
        .child(&dot)
        .popover(&popover)
        .css_classes(["flat", "label-btn"])
        .tooltip_text(gettext("Colour label"))
        .build();
    b.update_property(&[gtk::accessible::Property::Label(&gettext("Colour label"))]);
    b
}

/// The Photo / Grade / LUT Lab switch: icon tabs atop each side panel, bound to the page shown.
fn section_tabs(stack: &adw::ViewStack, panes: &[&gtk::Box]) {
    for pane in panes {
        let tabs = adw::ToggleGroup::builder().halign(gtk::Align::Center).margin_top(8).margin_bottom(2).build();
        for (name, icon, tip) in [
            ("photo", "camera-shutter-symbolic", gettext("Photo: develop, correct and crop (Alt+1)")),
            ("grade", "rawvibes-grade-symbolic", gettext("Grade: colour and looks (Alt+2)")),
            ("meta", "tag-outline-symbolic", gettext("Metadata: title, keywords, rights (Alt+3)")),
            ("lab", "rawvibes-lut-symbolic", gettext("LUT Lab: combine and convert LUTs (Alt+4)")),
        ] {
            tabs.add(adw::Toggle::builder().name(name).icon_name(icon).tooltip(&tip).build());
        }
        stack.bind_property("visible-child-name", &tabs, "active-name").bidirectional().sync_create().build();
        pane.prepend(&tabs);
    }
}

/// Card with a collapsible header, like the sections of a grading panel.
fn section(title: &str, child: &impl IsA<gtk::Widget>) -> gtk::Box {
    section_with(title, child, &[], true)
}

/// Section with an eye toggle that switches `part` off, after the `extra` header buttons (e.g. "As in camera").
fn eye_section(
    title: &str,
    child: &impl IsA<gtk::Widget>,
    open: bool,
    extra: &[gtk::Widget],
    sender: &ComponentSender<App>,
    part: Off,
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
        snd.input(Msg::Off(part, off));
    });
    EYES.with(|e| e.borrow_mut().push((part, eye.clone())));
    let mut header: Vec<gtk::Widget> = extra.to_vec();
    header.push(eye.upcast());
    section_with(title, child, &header, open)
}

/// "As in camera" button for a section header.
fn camera_button(tip: &str, msg: fn() -> Msg, sender: &ComponentSender<App>) -> gtk::Button {
    let b = header_button("camera-photo-symbolic", tip, msg, sender);
    b.set_valign(gtk::Align::Center);
    b.set_css_classes(&["flat", "circular"]);
    b
}

/// Heading of a sidebar group (Develop, Grade, Finish).
/// A phase of the workflow over the sections that follow ("Geometry", "Primaries"…), on the background.
fn phase_label(text: &str) -> gtk::Label {
    gtk::Label::builder().label(text.to_uppercase()).xalign(0.0).css_classes(["phase-label"]).build()
}

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

/// `d` developed `as_shot`, whole, fitting 1024 px, encoded in `profile`: what
/// the camera JPEG is matched against.
fn as_shot_render(d: &Decoded, as_shot: &Develop, profile: Profile) -> Arc<Photo> {
    let (w, h) = d.size();
    d.encode_fit(&Region::whole(w, h), 1024, profile, as_shot)
}

/// File extensions of the LUT files RawVibes reads.
const LUT_EXTENSIONS: [&str; 3] = ["cube", "vlt", "png"];

/// Settings key remembering where the camera LUT `name` was found.
fn camera_lut_key(name: &str) -> String {
    let safe: String =
        name.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    format!("camera-lut-{safe}")
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
    let group = adw::ToggleGroup::builder().homogeneous(true).build();
    for (name, label) in names {
        group.add(adw::Toggle::builder().name(*name).label(*label).build());
    }
    group
}

fn file_filters(name: &str, patterns: &[&str]) -> gio::ListStore {
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
.sidebar-handle { padding: 0 6px 0 10px; }
.sidebar-handle > box { min-width: 4px; min-height: 36px; border-radius: 2px; background-color: alpha(@borders, 0.7); transition: background-color 150ms ease; }
.sidebar-handle:hover > box, .sidebar-handle.dragging > box { background-color: @accent_bg_color; }
.section-header { padding: 6px 6px; margin-bottom: 2px; border-radius: 8px; }
.slider-row { margin: 2px 4px 0 4px; }
.slider-value { opacity: 0.8; padding: 0 5px; border-radius: 6px; transition: background-color 120ms ease; }
.slider-value:hover { opacity: 1; background-color: alpha(currentColor, 0.08); }
editablelabel.slider-value.editing { opacity: 1;
  background-color: color-mix(in srgb, var(--accent-bg-color) 16%, transparent);
  box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--accent-color) 70%, transparent); }
editablelabel.slider-value text { background: none; box-shadow: none; outline: none; padding: 0; min-height: 0;
  caret-color: var(--accent-color); }
editablelabel.slider-value text selection { background-color: color-mix(in srgb, var(--accent-bg-color) 55%, transparent); }
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
.star { padding: 4px 3px; min-width: 22px; }
.shot-card { padding: 12px; border-radius: 12px; background-image: linear-gradient(160deg, alpha(@accent_bg_color, 0.22), alpha(currentColor, 0.04)); }
.keyword { padding: 2px 10px; min-height: 0; font-size: 12px; background-color: alpha(@accent_bg_color, 0.18); }
.phase-label { font-size: 11px; font-weight: 700; letter-spacing: 0.06em; opacity: 0.62; margin: 8px 4px 0; }
.info-bar { min-height: 38px; padding: 0 10px; background-color: mix(@window_bg_color, black, 0.18); border-top: 1px solid alpha(black, 0.35); }
.info-bar .star image { color: @yellow_2; }
.label-red { color: #e5534b; }
.label-yellow { color: #e5c04b; }
.label-green { color: #57c46a; }
.label-blue { color: #5b8def; }
.label-purple { color: #a66be0; }
.label-dot { min-width: 12px; min-height: 12px; border-radius: 99px; background-color: currentColor; }
.label-dot.label-none, .label-btn.label-none .label-dot { background: none; box-shadow: inset 0 0 0 1.5px alpha(currentColor, 0.55); }
.roll-mark { opacity: 0.85; letter-spacing: 1px; }
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
    /// What the Photo tab shows (develop, crop, vignette: not in the LUT) and the Grade tab (all in the LUT).
    photo: gtk::Box,
    grade: gtk::Box,
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
    /// The look stack's widgets, moved between `look_box` and `applied_dock` by `Msg::Browse`.
    applied: gtk::Box,
    look_box: gtk::Box,
    applied_dock: gtk::ScrolledWindow,
    look_blend_toggle: adw::ToggleGroup,
    profile_sel: gtk::DropDown,
    /// The Grade tab's note of the input profile, linking to the Photo tab.
    profile_link: gtk::Label,
    /// The LUT browser, revealed under the look stack.
    /// The LUT browser page, shown in place of the sections (`side_stack`).
    browse: gtk::Box,
    side_stack: gtk::Stack,
    browse_btn: gtk::ToggleButton,
    develop: DevelopUi,
    /// What the Metadata tab shows.
    meta: MetaUi,
}

/// The Develop group (RAWs only).
struct DevelopUi {
    /// Set while `sync_develop` moves the controls: their signals are not edits.
    syncing: Rc<Cell<bool>>,
    /// Light and colour; `lens` and the Detail phase are apart: geometry goes between them.
    root: gtk::Box,
    lens: gtk::Box,
    detail_root: gtk::Box,
    /// Its profile row, the profile amounts (greyed without a profile) and the profile's CA switch.
    lens_profile: lens_ui::ProfileUi,
    lens_amounts: gtk::Box,
    lens_tca: adw::SwitchRow,
    preset: gtk::DropDown,
    kelvin: Slider,
    tint: Slider,
    picker: gtk::ToggleButton,
    exposure: Slider,
    detail: Vec<(Slider, DetailField)>,
    /// Idle without luminance NR: it only restores what that removed.
    nr_detail: Slider,
    /// Classic | AI; the AI hides luminance and detail and shows its strength.
    nr_mode: adw::ToggleGroup,
    nr_luma: Slider,
    nr_strength: Slider,
    /// While the AI runs: its progress and a stop button; why it cannot run.
    ai_busy: gtk::Box,
    ai_bar: gtk::ProgressBar,
    ai_msg: gtk::Label,
    ca: adw::SwitchRow,
}

/// Index of "Custom" in the white-balance preset dropdown (after As shot and the presets).
const WB_CUSTOM: u32 = WB_PRESETS.len() as u32 + 1;

fn build_develop(sender: &ComponentSender<App>) -> DevelopUi {
    let root = vbox(12);

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
    let dev_section = |title: &str, child: &gtk::Box, extra: &[gtk::Widget], part: DevPart| {
        eye_section(title, child, true, extra, sender, Off::Develop(part))
    };
    root.append(&phase_label(&gettext("Light and colour")));
    root.append(&dev_section(&gettext("White balance"), &wb, &[as_shot.upcast()], DevPart::WhiteBalance));

    let exp = vbox(4);
    let exposure = Slider::new(&gettext("Exposure"), -3.0, 3.0, 0.0, 2, None);
    exposure.connect(edit(|v| DevelopEdit::Exposure(v as f32)));
    exp.append(&exposure.root);
    let cam_ev = camera_button(
        &gettext("As in camera: the exposure the camera asks for"),
        || Msg::Develop(DevelopEdit::CameraExposure),
        sender,
    );
    root.append(&dev_section(&gettext("Exposure"), &exp, &[cam_ev.upcast()], DevPart::Exposure));

    let mut detail = Vec::new();
    let mut detail_slider = |bx: &gtk::Box, label: &str, range: (f64, f64), field: DetailField| {
        let digits = if range.1 - range.0 < 10.0 { 1 } else { 0 };
        let default = *field(&mut Detail::default()) as f64;
        let s = Slider::new(label, range.0, range.1, default, digits, None);
        let (snd, syncing) = (sender.clone(), syncing.clone());
        s.connect(move |v| {
            if !syncing.get() {
                snd.input(Msg::Develop(DevelopEdit::Detail(field, v as f32)));
            }
        });
        bx.append(&s.root);
        detail.push((s.clone(), field));
        s
    };
    let nr = vbox(4);
    let nr_mode = toggles(&[("classic", &gettext("Classic")), ("ai", &gettext("AI"))]);
    nr_mode
        .upcast_ref::<gtk::Widget>()
        .update_property(&[gtk::accessible::Property::Label(&gettext("Noise reduction method"))]);
    let (snd, sync) = (sender.clone(), syncing.clone());
    nr_mode.connect_active_name_notify(move |g| {
        if !sync.get() {
            snd.input(Msg::Develop(DevelopEdit::NrAi(g.active_name().as_deref() == Some("ai"))));
        }
    });
    nr.append(&nr_mode);
    let ai_bar = gtk::ProgressBar::builder().hexpand(true).valign(gtk::Align::Center).build();
    ai_bar.update_property(&[gtk::accessible::Property::Label(&gettext("AI noise reduction progress"))]);
    let stop = gtk::Button::builder()
        .icon_name("process-stop-symbolic")
        .tooltip_text(gettext("Stop and use classic noise reduction"))
        .css_classes(["flat", "circular"])
        .build();
    let snd = sender.clone();
    stop.connect_clicked(move |_| snd.input(Msg::Develop(DevelopEdit::NrAi(false))));
    let ai_busy = gtk::Box::builder().spacing(6).visible(false).build();
    ai_busy.append(&gtk::Label::builder().label(gettext("Denoising…")).css_classes(["caption"]).build());
    ai_busy.append(&ai_bar);
    ai_busy.append(&stop);
    nr.append(&ai_busy);
    let ai_msg =
        gtk::Label::builder().wrap(true).xalign(0.0).visible(false).css_classes(["caption", "warning"]).build();
    nr.append(&ai_msg);
    let nr_strength = detail_slider(&nr, &gettext("Strength"), (0.0, 100.0), |d| &mut d.nr_ai_strength);
    let nr_luma = detail_slider(&nr, &gettext("Luminance"), (0.0, 100.0), |d| &mut d.nr_luma);
    let nr_detail = detail_slider(&nr, &gettext("Detail"), (0.0, 100.0), |d| &mut d.nr_detail);
    // AI leaves some colour noise: chrominance stays in both methods.
    detail_slider(&nr, &gettext("Chrominance"), (0.0, 100.0), |d| &mut d.nr_chroma);
    let nr_sec = dev_section(&gettext("Noise reduction"), &nr, &[], DevPart::Noise);
    // Edge-aware, by area: the Grade tab's Highlights and Shadows are global curves.
    let tones = vbox(4);
    detail_slider(&tones, &gettext("Highlights"), (-100.0, 100.0), |d| &mut d.highlights);
    detail_slider(&tones, &gettext("Shadows"), (-100.0, 100.0), |d| &mut d.shadows);
    tones.append(&caption(&gettext(
        "Brightens or darkens whole areas and stops at their edges: no halos, detail kept.",
    )));
    root.append(&dev_section(&gettext("Shadows and highlights"), &tones, &[], DevPart::Tones));
    let haze = vbox(4);
    detail_slider(&haze, &gettext("Dehaze"), (-100.0, 100.0), |d| &mut d.dehaze);
    detail_slider(&haze, &gettext("Local contrast"), (0.0, 100.0), |d| &mut d.local_contrast);
    root.append(&dev_section(&gettext("Presence"), &haze, &[], DevPart::Presence));
    // Noise first, then sharpening dosed on the cleaned photo; both judged at 100% at the end.
    let detail_root = vbox(12);
    detail_root.append(&phase_label(&gettext("Detail")));
    detail_root.append(&nr_sec);
    let sharp = vbox(4);
    // Capture restores the lens's softness; output sharpening is the visible, creative kind.
    detail_slider(&sharp, &gettext("Capture sharpening"), (0.0, 100.0), |d| &mut d.sharpen);
    detail_slider(&sharp, &gettext("Capture radius"), (0.5, 2.0), |d| &mut d.sharpen_radius);
    detail_slider(&sharp, &gettext("Sharpening"), (0.0, 100.0), |d| &mut d.usm);
    detail_slider(&sharp, &gettext("Sharpening radius"), (0.5, 3.0), |d| &mut d.usm_radius);
    detail_slider(&sharp, &gettext("Sharpening threshold"), (0.0, 100.0), |d| &mut d.usm_threshold);
    detail_root.append(&dev_section(&gettext("Detail"), &sharp, &[], DevPart::Detail));
    // The lensfun profile, how much of it applies, then the manual corrections on top.
    let lens = vbox(4);
    let lens_profile = lens_ui::ProfileUi::new(sender);
    lens.append(&lens_profile.list);
    let lens_amounts = vbox(4);
    lens_amounts.set_margin_top(6);
    detail_slider(&lens_amounts, &gettext("Distortion"), (0.0, 100.0), |d| &mut d.lens_dist_amount);
    detail_slider(&lens_amounts, &gettext("Vignetting"), (0.0, 100.0), |d| &mut d.lens_vig_amount);
    let switch = |title: String, subtitle: String, edit: fn(bool) -> DevelopEdit| {
        let row = adw::SwitchRow::builder().title(title).subtitle(subtitle).build();
        let (snd, sync) = (sender.clone(), syncing.clone());
        row.connect_active_notify(move |r| {
            if !sync.get() {
                snd.input(Msg::Develop(edit(r.is_active())));
            }
        });
        row
    };
    let lens_tca =
        switch(gettext("Chromatic aberration"), gettext("The profile's lateral CA correction"), DevelopEdit::LensTca);
    let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();
    list.append(&lens_tca);
    lens_amounts.append(&list);
    lens.append(&lens_amounts);
    let manual = group_heading(&gettext("Manual corrections"));
    manual.set_margin_top(10);
    lens.append(&manual);
    detail_slider(&lens, &gettext("Distortion"), (-100.0, 100.0), |d| &mut d.lens_distortion);
    detail_slider(&lens, &gettext("Vignetting"), (-100.0, 100.0), |d| &mut d.lens_vignette);
    detail_slider(&lens, &gettext("Midpoint"), (-100.0, 100.0), |d| &mut d.lens_vignette_mid);
    detail_slider(&lens, &gettext("Red/cyan fringe"), (-100.0, 100.0), |d| &mut d.lens_ca_red);
    detail_slider(&lens, &gettext("Blue/yellow fringe"), (-100.0, 100.0), |d| &mut d.lens_ca_blue);
    detail_slider(&lens, &gettext("Purple fringe"), (0.0, 100.0), |d| &mut d.defringe);
    detail_slider(&lens, &gettext("Green fringe"), (0.0, 100.0), |d| &mut d.defringe_green);
    let ca =
        switch(gettext("Automatic chromatic aberration"), gettext("Removes red/cyan fringes"), DevelopEdit::CaCorrect);
    let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();
    list.append(&ca);
    lens.append(&list);
    // Placed above the crop by `build_sidebar`.
    let lens = dev_section(&gettext("Lens"), &lens, &[], DevPart::Lens);
    // Greyed out until a RAW opens (`sync_develop`).
    for w in [&root, &lens, &detail_root] {
        w.set_sensitive(false);
        w.set_tooltip_text(Some(&gettext("RAW development: open a RAW file to use it")));
    }
    DevelopUi {
        syncing,
        root,
        lens,
        detail_root,
        lens_profile,
        lens_amounts,
        lens_tca,
        preset,
        kelvin,
        tint,
        picker,
        exposure,
        detail,
        nr_detail,
        nr_mode,
        nr_luma,
        nr_strength,
        ai_busy,
        ai_bar,
        ai_msg,
        ca,
    }
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

/// The HDR section: the headroom of the HDR rendition and, on Linux, the HDR preview.
fn build_hdr(sender: &ComponentSender<App>) -> gtk::Box {
    let bx = vbox(6);
    let preview = adw::SwitchRow::builder()
        .title(gettext("HDR preview"))
        .subtitle(gettext("On an HDR screen, the brighter highlights the HDR exports carry"))
        .active(hdr_preview_setting())
        .build();
    let snd = sender.clone();
    preview.connect_active_notify(move |r| snd.input(Msg::HdrPreview(r.is_active())));
    let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();
    list.append(&preview);
    list.set_visible(cfg!(target_os = "linux"));
    bx.append(&list);
    let headroom = Slider::new(&gettext("Headroom"), 1.0, 3.0, export::HDR_HEADROOM as f64, 1, None);
    headroom.adj.set_value(export_ui::headroom() as f64);
    let snd = sender.clone();
    headroom.connect(move |v| snd.input(Msg::Headroom(v as f32)));
    bx.append(&headroom.root);
    bx.append(&caption(&gettext(
        "Stops above SDR white the highlights of a RAW or log photo reach in the HDR exports (JPEG HDR, JPEG XL, AVIF, PNG HDR). The SDR photo, the grade and the LUT stay as they are.",
    )));
    bx
}

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
    // Input profile: the photo's signal, so it comes before every edit; the Grade tab links here.
    let profile_box = vbox(6);
    let profile_sel = lab_page::profile_dropdown(Profile::Standard);
    profile_sel.update_property(&[gtk::accessible::Property::Label(&gettext("Input profile"))]);
    let hint = caption(&profile_hint(Profile::Standard));
    let profile_link = caption("");
    profile_link.set_markup(&profile_link_markup(Profile::Standard));
    let (snd, h, l) = (sender.clone(), hint.clone(), profile_link.clone());
    profile_sel.connect_selected_notify(move |d| {
        let profile = Profile::ALL[d.selected() as usize];
        h.set_label(&profile_hint(profile));
        l.set_markup(&profile_link_markup(profile));
        snd.input(Msg::Profile(profile));
    });
    profile_box.append(&profile_sel);
    profile_box.append(&hint);

    // The Photo and Grade tabs show one of these each (`App::init` swaps them).
    let photo = vbox(12);
    photo.append(&section(&gettext("Input profile"), &profile_box));
    photo.append(&section(&gettext("HDR"), &build_hdr(sender)));
    photo.append(&group_heading(&gettext("Not in the exported LUT")));
    let develop = build_develop(sender);
    // Most used first: light and colour, then geometry, detail judged at 100%, the vignette effect.
    // The pipeline's order is its own.
    photo.append(&develop.root);
    photo.append(&phase_label(&gettext("Geometry")));
    photo.append(&develop.lens);
    photo.append(crop);
    photo.append(&develop.detail_root);
    let grade = vbox(12);
    grade.set_visible(false);
    grade.append(&group_heading(&gettext("In the exported LUT")));
    grade.append(&profile_link);

    // Reference match comes first: the rest refines it.
    let matching = build_match(sender);
    let match_sec =
        eye_section(&gettext("Reference match"), &matching.stack, false, &[], sender, Off::Grade(Stage::Match));

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
    let warmth_sec = eye_section(&gettext("Warmth"), &wb, true, &[], sender, Off::Grade(Stage::WhiteBalance));

    let tone = vbox(4);
    add(&tone, &gettext("Exposure"), -3.0, 3.0, 2, None, |p| &mut p.exposure);
    add(&tone, &gettext("Contrast"), -100.0, 100.0, 0, None, |p| &mut p.contrast);
    add(&tone, &gettext("Highlights"), -100.0, 100.0, 0, None, |p| &mut p.highlights);
    add(&tone, &gettext("Shadows"), -100.0, 100.0, 0, None, |p| &mut p.shadows);
    add(&tone, &gettext("Whites"), -100.0, 100.0, 0, None, |p| &mut p.whites);
    add(&tone, &gettext("Blacks"), -100.0, 100.0, 0, None, |p| &mut p.blacks);
    let tone_sec = eye_section(&gettext("Tone"), &tone, true, &[], sender, Off::Grade(Stage::Tone));

    let (mixer_box, mixer) = build_mixer(sender);
    let mixer_sec = eye_section(&gettext("Channel mixer"), &mixer_box, false, &[], sender, Off::Grade(Stage::Mixer));

    let color = vbox(4);
    add(&color, &gettext("Saturation"), -100.0, 100.0, 0, None, |p| &mut p.saturation);
    add(&color, &gettext("Vibrance"), -100.0, 100.0, 0, None, |p| &mut p.vibrance);
    add(&color, &gettext("Shadow saturation"), -100.0, 100.0, 0, None, |p| &mut p.sat_shadows);
    add(&color, &gettext("Highlight saturation"), -100.0, 100.0, 0, None, |p| &mut p.sat_highlights);
    let color_sec = eye_section(&gettext("Colour"), &color, true, &[], sender, Off::Grade(Stage::Color));

    let film = vbox(4);
    add(&film, &gettext("Subtractive saturation"), -100.0, 100.0, 0, Some("track-film-sat"), |p| &mut p.film_sat);
    add(&film, &gettext("Colour density"), 0.0, 100.0, 0, Some("track-film-density"), |p| &mut p.film_density);
    film.append(&caption(&gettext(
        "Film makes colour by adding dye, so richer colours are also deeper. Subtractive saturation saturates the way dye does; density deepens saturated colours. Greys are untouched.",
    )));
    add(&film, &gettext("Soft light"), 0.0, 100.0, 0, None, |p| &mut p.soft_light);
    film.append(&caption(&gettext(
        "Soft light deepens shadows and lifts highlights, as RawTherapee's; 30 is its default.",
    )));
    let film_sec = eye_section(&gettext("Film colour"), &film, true, &[], sender, Off::Grade(Stage::Film));

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
    let selective_sec =
        eye_section(&gettext("Selective colour"), &hsl, true, &[], sender, Off::Grade(Stage::Selective));

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
    let hue_sec = eye_section(&gettext("Hue curves"), &hue_box, false, &[], sender, Off::Grade(Stage::HueCurves));

    // Primaries: lift / gamma / gain / offset wheels with a master slider each.
    let wheels_row = gtk::Grid::builder().column_spacing(10).row_spacing(10).column_homogeneous(true).build();
    let mut wheels = Vec::new();
    // What each wheel weighs most, as in DaVinci Resolve and the ASC CDL.
    let tips = [
        gettext("Lift: the shadows. Highlights barely move."),
        gettext("Gamma: the midtones. Black and white stay put."),
        gettext("Gain: the highlights. Black stays put."),
        gettext("Offset: the whole image by the same amount."),
    ];
    for (i, (name, tip)) in ["Lift", "Gamma", "Gain", "Offset"].into_iter().zip(tips).enumerate() {
        let col = vbox(4);
        col.set_tooltip_text(Some(&tip));
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
    let wheels_box = vbox(8);
    wheels_box.append(&wheels_row);
    wheels_box.append(&caption(&gettext(
        "Drag a wheel's centre towards a colour to tint that range, away from it to remove it; Luma brightens or darkens it. Lift is the shadows, Gamma the midtones, Gain the highlights, Offset everything. Double-click a wheel to reset it.",
    )));
    let wheels_sec =
        eye_section(&gettext("Primary colour wheels"), &wheels_box, true, &[], sender, Off::Grade(Stage::Wheels));

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
    let curves_sec = eye_section(&gettext("Curves"), &curve_box, true, &[], sender, Off::Grade(Stage::Curves));

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
    // The applied LUTs: in this section, or under the grid while the browser is open (`Msg::Browse`).
    let applied = vbox(8);
    applied.append(&look_blend);
    applied.append(&looks);
    look_box.append(&applied);
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
    // The LUT browser: a page of its own in place of the sections, one scroll for the whole grid.
    let browse = vbox(0);
    let back = gtk::Button::builder()
        .icon_name("go-previous-symbolic")
        .tooltip_text(gettext("Back to the grade (B)"))
        .css_classes(["flat"])
        .build();
    back.update_property(&[gtk::accessible::Property::Label(&gettext("Back to the grade"))]);
    let snd = sender.clone();
    back.connect_clicked(move |_| snd.input(Msg::Browse(false)));
    let browse_head = gtk::Box::builder().spacing(6).margin_start(8).margin_end(8).margin_top(6).build();
    browse_head.append(&back);
    browse_head.append(&gtk::Label::builder().label(gettext("LUTs")).css_classes(["heading"]).build());
    browse.append(&browse_head);
    let applied_dock = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(320)
        .margin_start(8)
        .margin_end(8)
        .margin_top(6)
        .margin_bottom(8)
        .build();
    // Hidden while the photo has no LUTs, so the grid keeps the whole page.
    applied_dock.set_visible(false);
    look_box.append(&caption(&gettext("Applied after the grade, top to bottom, and included in the exported LUT.")));
    let luts_sec = eye_section(&gettext("LUTs"), &look_box, true, &[], sender, Off::Looks);
    // A grading workflow, top to bottom: the look to grade under, balance, primaries, secondaries,
    // the look's finishing touches. The GPU's order is its own (the LUT comes last there).
    let phases = [
        (gettext("Look"), vec![&luts_sec]),
        (gettext("Balance"), vec![&match_sec, &warmth_sec]),
        (gettext("Primaries"), vec![&tone_sec, &curves_sec, &wheels_sec, &color_sec]),
        (gettext("Secondaries"), vec![&selective_sec, &hue_sec]),
        (gettext("Finishing"), vec![&film_sec, &mixer_sec]),
    ];
    for (phase, sections) in phases {
        grade.append(&phase_label(&phase));
        sections.into_iter().for_each(|s| grade.append(s));
    }

    let vig = vbox(4);
    add(&vig, &gettext("Amount"), -100.0, 100.0, 0, None, |p| &mut p.vignette);
    add(&vig, &gettext("Midpoint"), -100.0, 100.0, 0, None, |p| &mut p.vignette_mid);
    add(&vig, &gettext("Roundness"), -100.0, 100.0, 0, None, |p| &mut p.vignette_round);
    add(&vig, &gettext("Feather"), -100.0, 100.0, 0, None, |p| &mut p.vignette_feather);
    photo.append(&phase_label(&gettext("Effects")));
    photo.append(&eye_section(&gettext("Vignette"), &vig, false, &[], sender, Off::Grade(Stage::Vignette)));
    panel.append(&photo);
    panel.append(&grade);
    let meta = MetaUi::new(sender);
    meta.show_policy(&policy_name(export_policy()), &policy_hint(export_policy()));
    panel.append(&meta.root);
    meta.root.set_visible(false);
    meta.root.set_sensitive(false);

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
    let side_stack =
        gtk::Stack::builder().transition_type(gtk::StackTransitionType::SlideLeftRight).vexpand(true).build();
    side_stack.add_named(&scroll, Some("sections"));
    side_stack.add_named(&browse, Some("browser"));
    let body = vbox(0);
    body.append(&side_stack);
    // The section tabs go above `body`, and the pinned scopes atop it.
    let pane = vbox(0);
    pane.add_css_class("sidebar-pane");
    pane.append(&body);
    // Pinned: the scopes card leaves the scrolled panel for the top of the pane.
    let (p, sc) = (body.clone(), panel.clone());
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
        photo,
        grade,
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
        applied,
        look_box,
        applied_dock,
        browse,
        side_stack,
        browse_btn,
        develop,
        look_blend_toggle,
        profile_sel,
        profile_link,
        meta,
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
            .title("RawVibes")
            .build()
    }

    fn init(_: Self::Init, window: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        // Neutral dark surround is the colourist standard.
        adw::StyleManager::default().set_color_scheme(adw::ColorScheme::PreferDark);

        let snd = sender.clone();
        // Photo frames reach the monitor's colour space on the GPU thread, off the UI's.
        let (jobs, gpu_thread) = gpu::spawn(move |d| snd.input(Msg::Gpu(display_color::frames_to_monitor(d))));

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
        // Under the photo: its stars and label, what took it, and the filmstrip's collapse toggle.
        let photo_info = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["dim-label"])
            .build();
        let strip_btn = gtk::ToggleButton::builder()
            .icon_name("go-down-symbolic")
            .css_classes(["flat"])
            .valign(gtk::Align::Center)
            .build();
        let fs = filmstrip.root.clone();
        strip_btn.connect_toggled(move |b| {
            let hidden = b.is_active();
            fs.set_visible(!hidden);
            b.set_icon_name(if hidden { "go-up-symbolic" } else { "go-down-symbolic" });
            let tip = if hidden { gettext("Show the filmstrip") } else { gettext("Hide the filmstrip") };
            b.set_tooltip_text(Some(&tip));
            b.update_property(&[gtk::accessible::Property::Label(&tip)]);
            let _ = settings::set_value(settings::FILMSTRIP_HIDDEN, if hidden { "1" } else { "0" });
        });
        strip_btn.set_tooltip_text(Some(&gettext("Hide the filmstrip")));
        strip_btn.update_property(&[gtk::accessible::Property::Label(&gettext("Hide the filmstrip"))]);
        strip_btn.set_active(settings::value(settings::FILMSTRIP_HIDDEN).as_deref() == Some("1"));
        let info_bar = gtk::Box::builder().spacing(12).css_classes(["info-bar"]).build();
        info_bar.append(&photo_info);
        info_bar.append(&strip_btn);
        // Shown with the filmstrip's photos, whether the strip itself is collapsed or not.
        filmstrip.root.bind_property("reveal-child", &info_bar, "visible").sync_create().build();
        let image_page = vbox(0);
        image_page.append(&stage);
        image_page.append(&info_bar);
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
                &gettext("Drop camera RAWs, JPEGs, TIFFs or PNGs here, or choose files. Open several to edit them together.\nEvery grade can also be exported as a .cube LUT."),
                &gettext("Open Photo…"),
                || Msg::Open,
            ),
            Some("empty"),
        );
        grade_stack.add_named(&loading, Some("loading"));
        grade_stack.add_named(&image_page, Some("image"));

        let sidebar = build_sidebar(&sender, &crop_ui.section);
        // Crop editing ends (keeping the crop) once you move on: a click outside the Crop section,
        // or the section leaving the screen (another tab). The click still reaches what it was on.
        let finish = gtk::GestureClick::builder().button(0).propagation_phase(gtk::PropagationPhase::Capture).build();
        let (pane, section, edit) = (sidebar.pane.clone(), crop_ui.section.clone(), crop_ui.edit.clone());
        finish.connect_pressed(move |_, _, x, y| {
            if edit.is_active()
                && !pane.pick(x, y, gtk::PickFlags::DEFAULT).is_some_and(|w| w == section || w.is_ancestor(&section))
            {
                edit.set_active(false);
            }
        });
        sidebar.pane.add_controller(finish);
        let edit = crop_ui.edit.clone();
        crop_ui.section.connect_unmap(move |_| edit.set_active(false));
        // The sidebar's left edge drags to resize it; the width is remembered.
        // A visible pill in a wider grab area, as in RapidRAW.
        let handle =
            gtk::Box::builder().css_classes(["sidebar-handle"]).tooltip_text(gettext("Drag to resize")).build();
        handle.append(&gtk::Box::builder().valign(gtk::Align::Center).build());
        handle.set_cursor_from_name(Some("col-resize"));
        handle.update_property(&[gtk::accessible::Property::Label(&gettext("Resize the sidebar"))]);
        // Over the panel's left margin, so the pill sits in the middle of the gap before the cards.
        handle.set_halign(gtk::Align::Start);
        sidebar.pane.set_margin_start(10);
        let side = gtk::Overlay::builder().child(&sidebar.pane).build();
        side.add_overlay(&handle);
        let width = settings::value(settings::SIDEBAR_WIDTH).and_then(|w| w.parse().ok()).map(clamp_sidebar);
        let grade_split = adw::OverlaySplitView::builder()
            .sidebar_position(gtk::PackType::End)
            .min_sidebar_width(width.unwrap_or(340.0))
            .max_sidebar_width(width.unwrap_or(420.0))
            .sidebar_width_fraction(0.28)
            .content(&grade_stack)
            .sidebar(&side)
            .build();
        let drag = gtk::GestureDrag::new();
        let (split, h) = (grade_split.clone(), handle.clone());
        drag.connect_drag_begin(move |_, _, _| h.add_css_class("dragging"));
        let (sp, h) = (split.clone(), handle.clone());
        drag.connect_drag_update(move |g, _, _| {
            // From the pointer's place in the split view, not the offset from the handle:
            // the handle moves with the width, and an offset from it would feed back (flicker).
            let Some(at) =
                g.point(None).and_then(|(x, y)| h.compute_point(&sp, &gtk::graphene::Point::new(x as f32, y as f32)))
            else {
                return;
            };
            let w = clamp_sidebar(sp.width() as f64 - at.x() as f64);
            sp.set_min_sidebar_width(w);
            sp.set_max_sidebar_width(w);
        });
        let h = handle.clone();
        drag.connect_drag_end(move |_, _, _| {
            h.remove_css_class("dragging");
            let _ = settings::set_value(settings::SIDEBAR_WIDTH, &format!("{:.0}", split.max_sidebar_width()));
        });
        handle.add_controller(drag);

        // ---------------- LUT browser
        let flow = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            // A click or Enter adds; hovering previews.
            .activate_on_single_click(true)
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
            .icon_name("rawvibes-lut-symbolic")
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
                .vexpand(true)
                .build(),
            Some("grid"),
        );
        let gallery = vbox(0);
        gallery.append(&folder_row);
        gallery.append(&groups);
        gallery.append(&search);
        gallery.append(&gallery_stack);

        gallery.set_vexpand(true);
        gallery_stack.set_vexpand(true);
        sidebar.browse.append(&gallery);
        sidebar.browse.append(&sidebar.applied_dock);
        // Once shown, the grid takes the keyboard, so arrows browse at once (Ctrl+F's search keeps it).
        let (f, browse) = (flow.clone(), sidebar.browse.clone());
        sidebar.side_stack.connect_visible_child_name_notify(move |st| {
            if st.visible_child_name().as_deref() != Some("browser") {
                return;
            }
            let inside = browse.root().and_then(|r| r.focus()).is_some_and(|f| f.is_ancestor(&browse));
            if !inside {
                f.child_at_index(0).map_or_else(|| f.grab_focus(), |c| c.grab_focus());
            }
        });

        // ---------------- view stack + header
        let stack = adw::ViewStack::new();
        // Photo and Grade share the canvas: it moves to whichever of the two is shown.
        let tab = || gtk::Box::builder().orientation(gtk::Orientation::Vertical).vexpand(true).hexpand(true).build();
        let (photo_tab, grade_tab) = (tab(), tab());
        photo_tab.append(&grade_split);
        stack.add_titled_with_icon(&photo_tab, Some("photo"), &gettext("Photo"), "camera-shutter-symbolic");
        stack.add_titled_with_icon(&grade_tab, Some("grade"), &gettext("Grade"), "rawvibes-grade-symbolic");
        let meta_tab = tab();
        stack.add_titled_with_icon(&meta_tab, Some("meta"), &gettext("Metadata"), "tag-outline-symbolic");
        let st = stack.clone();
        sidebar.profile_link.connect_activate_link(move |_, _| {
            st.set_visible_child_name("photo");
            glib::Propagation::Stop
        });
        let lab = lab_page::build(&sender, &export_menu("lab-export", false));
        stack.add_titled_with_icon(&lab.root, Some("lab"), &gettext("LUT Lab"), "rawvibes-lut-symbolic");
        let snd = sender.clone();
        let (split, photo, grade, meta) =
            (grade_split.clone(), sidebar.photo.clone(), sidebar.grade.clone(), sidebar.meta.root.clone());
        let browse_btn = sidebar.browse_btn.clone();
        stack.connect_visible_child_name_notify(move |s| {
            let name = s.visible_child_name();
            // The browser belongs to the Grade tab: another tab closes it.
            if name.as_deref() != Some("grade") {
                browse_btn.set_active(false);
            }
            if let Some(tab) = s.visible_child().and_downcast::<gtk::Box>().filter(|_| name.as_deref() != Some("lab"))
                && split.parent().as_ref() != Some(tab.upcast_ref())
            {
                split.unparent();
                tab.append(&split);
            }
            photo.set_visible(name.as_deref() == Some("photo"));
            grade.set_visible(name.as_deref() == Some("grade"));
            meta.set_visible(name.as_deref() == Some("meta"));
            let page = match name.as_deref() {
                Some("lab") => Page::Lab,
                _ => Page::Grade,
            };
            snd.input(Msg::Page(page));
        });

        let header = adw::HeaderBar::new();
        let title = adw::WindowTitle::new("RawVibes", "");
        header.set_title_widget(Some(&title));
        section_tabs(&stack, &[&sidebar.pane, &lab.pane]);

        let open = gtk::Button::builder()
            .icon_name("document-open-symbolic")
            .tooltip_text(gettext("Open Photo (Ctrl+O)"))
            .build();
        let snd = sender.clone();
        open.connect_clicked(move |_| snd.input(Msg::Open));
        header.pack_start(&open);
        let history = gtk::Box::builder().css_classes(["linked"]).build();
        let undo = header_button("edit-undo-symbolic", &gettext("Undo (Ctrl+Z)"), || Msg::Undo, &sender);
        let redo = header_button("edit-redo-symbolic", &gettext("Redo (Ctrl+Shift+Z)"), || Msg::Redo, &sender);
        for b in [&undo, &redo] {
            b.set_sensitive(false);
            history.append(b);
        }
        header.pack_start(&history);
        let marks = gtk::Box::builder().spacing(0).build();
        let stars: Vec<gtk::Button> = (1..=5u8)
            .map(|n| {
                let tip = gettext!("{} stars ({})", n, n);
                let b = header_button("non-starred-symbolic", &tip, move || Msg::Rate(n, true), &sender);
                b.add_css_class("flat");
                b.add_css_class("star");
                marks.append(&b);
                b
            })
            .collect();
        let label_btn = label_menu(&sender);
        marks.append(&label_btn);
        info_bar.prepend(&marks);
        let chart_btn = gtk::ToggleButton::builder()
            .icon_name("applications-science-symbolic")
            .tooltip_text(gettext("Test chart: shows what the LUT does to neutrals, hues and skin (T)"))
            .build();
        let snd = sender.clone();
        chart_btn.connect_toggled(move |b| snd.input(Msg::Chart(b.is_active())));
        let tools = gtk::Box::builder().css_classes(["linked"]).build();
        tools.append(&chart_btn);
        let presets = presets_ui::build(&sender);
        tools.append(&presets.button);
        let camera_btn = gtk::Button::builder()
            .icon_name("camera-photo-symbolic")
            .tooltip_text(gettext("As in camera: white balance and exposure as shot"))
            .sensitive(false)
            .build();
        let snd = sender.clone();
        camera_btn.connect_clicked(move |_| snd.input(Msg::AsInCamera(CameraPart::All)));
        tools.append(&camera_btn);

        let menu = gio::Menu::new();
        let bg_menu = gio::Menu::new();
        bg_menu.append(Some(gettext("Default").as_str()), Some("win.background::default"));
        bg_menu.append(Some(gettext("Black").as_str()), Some("win.background::black"));
        bg_menu.append(Some(gettext("White").as_str()), Some("win.background::white"));
        menu.append_section(Some(gettext("Preview background").as_str()), &bg_menu);
        let edits_menu = gio::Menu::new();
        edits_menu.append(Some(gettext("Copy Edits").as_str()), Some("win.copy-edits"));
        edits_menu.append(Some(gettext("Paste Edits").as_str()), Some("win.paste-edits"));
        edits_menu.append(Some(gettext("Paste Edits to All Photos").as_str()), Some("win.paste-edits-all"));
        edits_menu.append(Some(gettext("Reset All Edits").as_str()), Some("win.reset-all"));
        menu.append_section(None, &edits_menu);
        let merge_menu = gio::Menu::new();
        merge_menu.append(Some(gettext("Merge Brackets to HDR…").as_str()), Some("win.merge-hdr"));
        menu.append_section(None, &merge_menu);
        let section_menu = gio::Menu::new();
        section_menu.append(Some(gettext("Preferences").as_str()), Some("win.preferences"));
        section_menu.append(Some(gettext("Keyboard Shortcuts").as_str()), Some("win.show-help-overlay"));
        section_menu.append(Some(gettext("About RawVibes").as_str()), Some("win.about"));
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
            .label(gettext("Export"))
            .menu_model(&export_menu)
            .css_classes(["suggested-action"])
            .tooltip_text(gettext("Export the edited photo (Ctrl+E)"))
            .dropdown_tooltip(gettext("Photo and LUT formats"))
            .sensitive(false)
            .build();
        let snd = sender.clone();
        export.connect_clicked(move |_| snd.input(Msg::ExportPhoto(None)));
        header.pack_end(&export);

        let compare_btn = gtk::ToggleButton::builder()
            .icon_name("view-dual-symbolic")
            .tooltip_text(gettext("Before / after (\\)"))
            .build();
        let snd = sender.clone();
        compare_btn.connect_toggled(move |b| snd.input(Msg::Compare(b.is_active())));
        let copy =
            header_button("edit-copy-symbolic", &gettext("Copy edits (Ctrl+Shift+C)"), || Msg::CopyEdits, &sender);
        let paste =
            header_button("edit-paste-symbolic", &gettext("Paste edits (Ctrl+Shift+V)"), || Msg::PasteEdits, &sender);
        let paste_all = header_button(
            "edit-select-all-symbolic",
            &gettext("Paste edits to every photo in the filmstrip, crops kept"),
            || Msg::PasteEditsAll,
            &sender,
        );
        paste.set_sensitive(false);
        paste_all.set_sensitive(false);
        let full = header_button("view-fullscreen-symbolic", &gettext("Fullscreen (F11)"), || Msg::Fullscreen, &sender);
        let edit = gtk::Box::builder().css_classes(["linked"]).build();
        for b in [
            compare_btn.upcast_ref::<gtk::Widget>(),
            copy.upcast_ref(),
            paste.upcast_ref(),
            paste_all.upcast_ref(),
            full.upcast_ref(),
        ] {
            edit.append(b);
        }
        header.pack_end(&edit);
        header.pack_end(&tools);

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
        for w in [
            tools.upcast_ref::<gtk::Widget>(),
            marks.upcast_ref(),
            copy.upcast_ref(),
            paste_all.upcast_ref(),
            full.upcast_ref(),
        ] {
            bp.add_setter(w, "visible", Some(&false.to_value()));
        }
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
        let action = gio::SimpleAction::new("merge-hdr", None);
        let snd = sender.clone();
        action.connect_activate(move |_, _| snd.input(Msg::MergeHdr));
        window.add_action(&action);
        let action = gio::SimpleAction::new("export-photo", Some(glib::VariantTy::STRING));
        let snd = sender.clone();
        action.connect_activate(move |_, v| {
            let format = v.and_then(|v| v.str().and_then(PhotoFormat::from_tag));
            snd.input(Msg::ExportPhoto(format));
        });
        window.add_action(&action);
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
                .application_name("RawVibes")
                .application_icon(APP_ID)
                .version(env!("CARGO_PKG_VERSION"))
                .comments(gettext(
                    "RAW development, colour grading and 3D .cube LUT creation, GPU accelerated (Vulkan/Metal).",
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
                "Faster previews are softer while you drag. Automatic measures this computer's graphics card and keeps dragging smooth.",
            ))
            .model(&gtk::StringList::new(&labels))
            .selected(PreviewQuality::ALL.iter().position(|q| *q == PreviewQuality::stored()).unwrap_or(0) as u32)
            .build();
        let snd = sender.clone();
        quality_row.connect_selected_notify(move |r| snd.input(Msg::Quality(r.selected())));
        let view_group = adw::PreferencesGroup::builder().title(gettext("RAW development")).build();
        view_group.add(&quality_row);
        let refine_row = adw::SwitchRow::builder()
            .title(gettext("Sharpen the preview when you stop"))
            .subtitle(gettext(
                "Renders the full-quality frame after each change. Turn off on slower computers to keep editing responsive.",
            ))
            .active(settings::value(settings::REFINE_IDLE).as_deref() != Some("0"))
            .build();
        let snd = sender.clone();
        refine_row.connect_active_notify(move |r| snd.input(Msg::Refine(r.is_active())));
        view_group.add(&refine_row);
        let camera_row = adw::SwitchRow::builder()
            .title(gettext("Open RAWs as in camera"))
            .subtitle(gettext("A RAW you have not edited yet starts from the camera's white balance and exposure."))
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
        prefs.add(&metadata_prefs(&sender));
        for (name, msg) in [
            ("copy-edits", (|| Msg::CopyEdits) as fn() -> Msg),
            ("paste-edits", || Msg::PasteEdits),
            ("paste-edits-all", || Msg::PasteEditsAll),
            ("reset-all", || Msg::ResetAll),
        ] {
            let action = gio::SimpleAction::new(name, None);
            let snd = sender.clone();
            action.connect_activate(move |_, _| snd.input(msg()));
            window.add_action(&action);
        }
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
        for (accel, msg) in [
            ("<Control>z", (|| Msg::Undo) as fn() -> Msg),
            ("<Control><Shift>z", || Msg::Redo),
            ("<Control>y", || Msg::Redo),
            ("<Control><Shift>c", || Msg::CopyEdits),
            ("<Control><Shift>v", || Msg::PasteEdits),
            ("F11", || Msg::Fullscreen),
        ] {
            let snd = sender.clone();
            shortcut(accel, Box::new(move || snd.input(msg())));
        }
        for n in 0..=5u8 {
            let snd = sender.clone();
            shortcut(&n.to_string(), Box::new(move || snd.input(Msg::Rate(n, false))));
        }
        for (key, label) in [("6", Label::Red), ("7", Label::Yellow), ("8", Label::Green), ("9", Label::Blue)] {
            let snd = sender.clone();
            shortcut(key, Box::new(move || snd.input(Msg::Mark(label))));
        }
        let e = export.clone();
        shortcut("<Control>e", Box::new(move || e.emit_clicked()));
        let snd = sender.clone();
        shortcut("<Control><Shift>e", Box::new(move || snd.input(Msg::Export(Format::Cube, 33, What::Grade))));
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
        shortcut("<Alt>1", Box::new(move || s.set_visible_child_name("photo")));
        let s = stack.clone();
        shortcut("<Alt>2", Box::new(move || s.set_visible_child_name("grade")));
        let s = stack.clone();
        shortcut("<Alt>3", Box::new(move || s.set_visible_child_name("meta")));
        let s = stack.clone();
        shortcut("<Alt>4", Box::new(move || s.set_visible_child_name("lab")));
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
            snd.input(Msg::RollAddPaths(paths, None));
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
            off: Vec::new(),
            meta: Meta::default(),
            lens_profile: None,
            lens_db: None,
            source_meta: None,
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
            gpu_thread: Some(gpu_thread),
            exporting: None,
            export_frame: None,
            export_gen: 0,
            hdr_preview: hdr_preview_setting(),
            headroom: export_ui::headroom(),
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
            frame_cost: 0.0,
            auto_draft: 0.75,
            refine: settings::value(settings::REFINE_IDLE).as_deref() != Some("0"),
            develop_tick: 0,
            camera_match: None,
            camera_curve: None,
            camera_jpeg: None,
            camera_job: None,
            saved: None,
            save_tick: 0,
            looks_loading: false,
            looks_gen: 0,
            save_failed: false,
            encode_again: false,
            encoded: None,
            rating: 0,
            label: Label::None,
            history: Vec::new(),
            undo_at: 0,
            clipboard: None,
            batch: None,
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
                export_photo: export_ui::build(&sender),
                chart_btn,
                camera_btn,
                title,
                undo,
                redo,
                stars,
                label_btn,
                photo_info,
                paste,
                paste_all,
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
        // Pick up where the last session left: its filmstrip and open photo (files since moved are skipped).
        let roll: Vec<PathBuf> = settings::value(settings::ROLL)
            .unwrap_or_default()
            .lines()
            .map(PathBuf::from)
            .filter(|p| p.is_file())
            .collect();
        if !roll.is_empty() {
            let last = settings::value(settings::LAST_PHOTO).map(PathBuf::from);
            sender.input(Msg::RollAddPaths(roll, last));
        }
        if let Some(dir) = settings::folder(settings::LUT_FOLDER) {
            sender.input(Msg::Folder(dir));
        }
        // 3 MB of JSON: read off the main thread.
        let s = sender.input_sender().clone();
        std::thread::spawn(move || s.emit(Msg::LensDb(lensdb::Db::load(&lens_db_path()).ok().map(Arc::new))));
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
                        s.input(Msg::RollAddPaths(paths, None));
                    }
                });
            }
            Msg::RollAddPaths(paths, open_at) => self.add_to_roll(paths, open_at, &sender),
            Msg::MergeHdr => {
                let dialog = photo_dialog(&gettext("Merge Brackets to HDR"));
                let s = sender.clone();
                dialog.open_multiple(Some(&self.ui.window), gio::Cancellable::NONE, move |r| {
                    let Ok(files) = r else { return };
                    let paths = files.iter::<gio::File>().filter_map(|f| f.ok().and_then(|f| f.path())).collect();
                    s.input(Msg::MergeHdrRun(paths));
                });
            }
            Msg::MergeHdrRun(mut paths) => {
                if paths.len() < 2 {
                    return self.toast(&gettext("Pick two or more RAWs of the same scene at different exposures"));
                }
                paths.sort();
                let first = &paths[0];
                let stem = first.file_stem().unwrap_or_default().to_string_lossy();
                let out = free_path(first.parent().unwrap_or(std::path::Path::new(".")), &stem, "dng", "-HDR");
                self.toast(&gettext!("Merging {} photos to HDR…", paths.len()));
                let s = sender.input_sender().clone();
                std::thread::spawn(move || {
                    let refs: Vec<&std::path::Path> = paths.iter().map(PathBuf::as_path).collect();
                    s.emit(Msg::MergeHdrDone(rawvibes_core::merge::merge_files(&refs, &out).map(|_| out)));
                });
            }
            Msg::MergeHdrDone(Ok(path)) => {
                self.toast(&gettext!("HDR merge saved: {}", path.file_name().unwrap_or_default().to_string_lossy()));
                self.add_to_roll(vec![path], None, &sender);
            }
            Msg::MergeHdrDone(Err(e)) => self.toast(&gettext!("HDR merge failed: {}", e)),
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
                    self.ui.window.set_title(Some("RawVibes"));
                    self.ui.title.set_title("RawVibes");
                    (self.history, self.undo_at) = (Vec::new(), 0);
                    self.sync_undo();
                    (self.meta, self.source_meta) = (Meta::default(), None);
                    self.sync_meta();
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
            Msg::SaveSidecar(tick) if tick == self.save_tick => {
                self.record_edits();
                self.save_now();
            }
            Msg::SaveSidecar(_) => {}
            Msg::WriteSidecar(tick) if tick == self.save_tick => self.save_now(),
            Msg::WriteSidecar(_) => {}
            Msg::Close => {
                self.save_now();
                // A running export would hold the join (and the frozen window) until it is written.
                if let Some(cancel) = &self.exporting {
                    cancel.store(true, Ordering::Relaxed);
                }
                // The GPU worker ends with its channel; NVIDIA's exit handlers segfault if it is still tearing down.
                self.jobs = mpsc::channel().0;
                self.ui.window.set_visible(false);
                if let Some(t) = self.gpu_thread.take() {
                    let _ = t.join();
                }
                self.ui.window.destroy();
            }
            // Another photo was opened, or the looks reset, meanwhile.
            Msg::SidecarLooks(generation, _) if generation != self.looks_gen => {}
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
                self.reset_history();
                self.batch_ready(&sender);
            }
            Msg::AsInCamera(part) => self.as_in_camera(part, &sender),
            Msg::CameraCurve(path, ..) if Some(&path) != self.path.as_ref() => {}
            Msg::CameraCurve(photo, jpeg, curve) => {
                self.camera_jpeg = jpeg.map(|j| (photo.clone(), j));
                let Some(curve) = curve else {
                    return self.toast(&gettext("No camera JPEG to match the tone curve to"));
                };
                let before = self.grade.clone();
                let mut grade = self.grade.clone();
                grade.curves[MASTER] = curve.clone();
                self.camera_curve = Some((photo, curve));
                self.apply_grade(grade);
                self.undo_toast(&gettext("Tone curve matched to the camera JPEG"), move || {
                    Msg::RestoreGrade(Box::new(before.clone()))
                });
            }
            Msg::CameraLut(path, _, _) if Some(&path) != self.path.as_ref() => {}
            Msg::CameraLut(photo, l, Some(file)) => {
                let title = gettext!("The camera applied the LUT “{}” at {}%", l.name, l.opacity);
                let msg = move || Msg::CameraLutAdd(photo.clone(), l.clone(), file.clone());
                self.action_toast(&title, &gettext("Use LUT"), msg);
            }
            Msg::CameraLut(photo, l, None) => {
                let title = gettext!("The camera applied the LUT “{}”: not in your LUT folder", l.name);
                self.action_toast(&title, &gettext("Find…"), move || Msg::CameraLutFind(photo.clone(), l.clone()));
            }
            Msg::CameraLutFind(photo, l) => {
                let dialog = gtk::FileDialog::builder()
                    .title(gettext!("Find the LUT “{}”", l.name))
                    .filters(&file_filters(&gettext("LUTs"), &LUT_EXTENSIONS))
                    .modal(true)
                    .build();
                dialog.set_initial_folder(start_in(settings::LUT_FOLDER).as_ref());
                let s = sender.clone();
                dialog.open(Some(&self.ui.window), gio::Cancellable::NONE, move |r| {
                    if let Some(p) = r.ok().and_then(|f| f.path()) {
                        // Remembered by name: the next photo with this LUT finds it.
                        let _ = settings::set_value(&camera_lut_key(&l.name), &p.to_string_lossy());
                        s.input(Msg::CameraLutAdd(photo, l, p));
                    }
                });
            }
            Msg::CameraLutAdd(photo, ..) if Some(&photo) != self.path.as_ref() => {}
            Msg::CameraLutAdd(photo, l, p) => {
                // The JPEG the curve was matched to already carries this LUT: it replaces the curve.
                if self.camera_curve.take().is_some_and(|(f, c)| f == photo && c == self.grade.curves[MASTER]) {
                    let mut grade = self.grade.clone();
                    grade.curves[MASTER] = neutral_curve(MASTER);
                    self.apply_grade(grade);
                }
                // Real Time LUTs take V-Log: the camera applies them to its log signal.
                let known = Some((Profile::VLog, l.opacity as f32 / 100.0));
                Self::load_layer(Layers::Look, p, known, &sender);
            }
            Msg::CameraJpeg(path, _) if Some(&path) != self.path.as_ref() => {}
            Msg::CameraJpeg(path, Ok(reference)) => {
                let (Some(d), Some(as_shot)) = (self.decoded.clone(), self.as_shot()) else { return };
                self.camera_jpeg = Some((path.clone(), reference.clone()));
                // The source is the photo as the camera shot it, before any edit.
                let source = as_shot_render(&d, &as_shot, self.profile);
                let m = &self.ui.sidebar.matching;
                m.pic.set_paintable(Some(&texture(
                    reference.width,
                    reference.height,
                    display_color::to_monitor(rgba8(&reference.pixels)),
                )));
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
                    self.send(Job::SampleRaw { at, radius: 4, detail: Box::new(self.applied_develop().detail) });
                }
            }
            Msg::DevelopSettled(tick) if tick == self.develop_tick && self.drafting && self.refine => {
                self.drafting = false;
                self.encode_photo(&sender);
                self.request_view();
            }
            Msg::DevelopSettled(_) => {}
            Msg::AiReady => {
                self.encode_photo(&sender);
                self.request_view();
            }
            Msg::Refine(on) => {
                self.refine = on;
                if let Err(e) = settings::set_value(settings::REFINE_IDLE, if on { "1" } else { "0" }) {
                    self.toast(&gettext!("Cannot save the setting: {}", e));
                }
                if on && std::mem::take(&mut self.drafting) {
                    self.encode_photo(&sender);
                    self.request_view();
                }
            }
            Msg::Quality(i) => {
                self.quality = PreviewQuality::ALL[i as usize % PreviewQuality::ALL.len()];
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
                let left = self.replace_looks(Vec::new(), &sender);
                let graded = self.graded();
                let previous = self.path.as_ref().and_then(|p| self.roll.iter_mut().find(|it| it.path == *p));
                if let Some(it) = previous {
                    it.grade = graded;
                    it.looks = left;
                }
                self.apply_grade(Grade::default());
                self.ui.sidebar.look_blend_toggle.set_active_name(Some("chain"));
                self.ui.sidebar.develop.picker.set_active(false);
                self.ui.window.set_title(Some(&format!("{} — RawVibes", file_label(&path))));
                self.zoom = None;
                self.reset_crop();
                self.decoded = Some(decoded);
                self.path = Some(path);
                self.develop = Develop::default();
                (self.rating, self.label, self.meta) = (0, Label::None, Meta::default());
                self.lens_profile = None;
                self.drafting = false;
                self.sync_develop();
                let path = self.path.clone().expect("set above");
                // The sidecar sets the eyes itself: resetting them first would echo each one twice.
                match Sidecar::load(&path) {
                    Ok(Some(sc)) => self.restore_sidecar(sc, &sender),
                    Ok(None) => {
                        self.set_offs(&[]);
                        if self.camera().is_some() && settings::value(settings::AS_IN_CAMERA).as_deref() == Some("1") {
                            sender.input(Msg::AsInCamera(CameraPart::All));
                        }
                    }
                    Err(e) => {
                        self.set_offs(&[]);
                        self.toast(&gettext!("Cannot read the edits saved for {}: {}", file_label(&path), e))
                    }
                }
                self.sync_meta();
                self.update_lens(&sender, false);
                // Left earlier this session: that stack is newer than the sidecar's and keeps path-less layers.
                if let Some(looks) =
                    self.roll.iter().find(|it| it.path == path).map(|it| it.looks.clone()).filter(|l| !l.is_empty())
                {
                    self.replace_looks(looks, &sender);
                }
                // Nothing is written until something changes from what was just read.
                self.saved = Some(self.sidecar_state());
                self.show_marks();
                self.ui.title.set_title(&file_label(&path));
                self.reset_history();
                self.batch_ready(&sender);
                // Opening a photo means you want to see it.
                if self.show_chart {
                    self.show_chart = false;
                    self.ui.chart_btn.set_active(false);
                }
                self.encode_photo(&sender);
            }
            // RollLoaded already reported the error and took the photo out of the
            // strip; the photo still on screen (if any) stays the one edited.
            Msg::Loaded(_, path, Err(e)) => {
                self.active = self.path.as_ref().and_then(|p| self.roll.iter().position(|it| it.path == *p));
                self.ui.filmstrip.set_active(self.active);
                self.show_source();
                if let Some(b) = self.batch.as_mut().filter(|b| b.waiting.as_ref() == Some(&path)) {
                    b.waiting = None;
                    b.failed.push(format!("{}: {e}", file_label(&path)));
                    self.batch_next(&sender);
                }
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
                let before = (self.sidecar_state(), self.looks.clone());
                let develop = self.develop;
                (self.develop, self.lens_profile) = (Develop::default(), None);
                // The photo's own lens profile is not an edit: it comes back.
                self.update_lens(&sender, false);
                if self.develop != develop {
                    self.sync_develop();
                    self.develop_changed(&sender);
                }
                self.apply_grade(Grade::default());
                self.reset_crop();
                self.crop_changed(&sender);
                self.ui.sidebar.look_blend_toggle.set_active_name(Some("chain"));
                self.replace_looks(Vec::new(), &sender);
                self.set_offs(&[]);
                self.undo_toast(&gettext("All edits cleared"), move || Msg::RestoreEdits(Box::new(before.clone())));
            }
            Msg::RestoreEdits(edits) => self.restore_edits(*edits, &sender),
            Msg::MetaEdit(f, text) => {
                if self.path.is_some() && meta_ui::apply(&mut self.meta, f, &text) {
                    self.schedule_save();
                    // The text rows show what was typed; keyword chips are drawn from the photo's list.
                    if matches!(f, MetaField::Keywords) {
                        self.ui.sidebar.meta.set(&self.meta, &author_defaults());
                    }
                }
            }
            Msg::MetaDefaults => self.sync_meta(),
            Msg::LensDb(db) => {
                self.lens_db = db;
                self.update_lens(&sender, true);
            }
            Msg::LensSearch(text) => {
                let found = self.lens_db.as_deref().map_or_else(Vec::new, |db| {
                    db.search(&text).into_iter().map(|l| (l.maker.clone(), l.model.clone())).collect()
                });
                self.ui.sidebar.develop.lens_profile.results(found);
            }
            Msg::LensPick(pick) => {
                let LensView { focal, aperture, .. } = self.find_lens();
                self.lens_profile = pick.map(|(maker, model)| LensChoice { maker, model, focal, aperture });
                self.update_lens(&sender, true);
                self.schedule_save();
            }
            Msg::LensManual(focal, aperture) => {
                // Typed for the automatic lens: it becomes the chosen one, with these values.
                if self.lens_profile.is_none()
                    && let Some((maker, model)) = self.find_lens().lens
                {
                    self.lens_profile = Some(LensChoice { maker, model, ..Default::default() });
                }
                if let Some(c) = self.lens_profile.as_mut() {
                    (c.focal, c.aperture) = (focal, aperture);
                }
                self.update_lens(&sender, true);
                self.schedule_save();
            }
            Msg::SourceMeta(generation, ..) if generation != self.load_gen => {}
            Msg::SourceMeta(_, path, m) => {
                self.source_meta = m.map(|m| (path, m));
                self.ui.sidebar.meta.show_camera(self.camera_meta());
                self.update_lens(&sender, true);
            }
            Msg::Undo => self.step_history(true, &sender),
            Msg::Redo => self.step_history(false, &sender),
            Msg::Rate(..) | Msg::Mark(_) if self.path.is_none() => {}
            Msg::Rate(r, toggle) => {
                self.rating = if toggle && r == self.rating { 0 } else { r };
                self.show_marks();
                self.schedule_save();
            }
            Msg::Mark(l) => {
                self.label = l;
                self.show_marks();
                self.schedule_save();
            }
            Msg::RollFilter => self.show_marks(),
            Msg::CopyEdits if self.path.is_some() => {
                self.clipboard = Some((self.sidecar_state(), self.looks.clone()));
                self.ui.paste.set_sensitive(true);
                self.ui.paste_all.set_sensitive(self.roll.len() > 1);
                self.toast(&gettext("Edits copied"));
            }
            Msg::CopyEdits => {}
            Msg::PasteEdits => {
                let Some((sc, looks)) = self.clipboard.clone().filter(|_| self.path.is_some()) else { return };
                self.record_edits();
                let sc = pasted(&sc, &self.sidecar_state());
                self.restore_edits((sc, looks), &sender);
                self.toast(&gettext("Edits pasted: Ctrl+Z undoes it"));
            }
            Msg::PasteEditsAll => {
                let Some(clip) = self.clipboard.clone() else { return };
                // The open photo too, unless the filter hides it.
                if self.path.is_some() && self.ui.filmstrip.shows(self.rating, self.label) {
                    sender.input(Msg::PasteEdits);
                }
                match self.paste_all(&clip) {
                    (n, failed) if failed.is_empty() => {
                        self.toast(&gettext!("Edits pasted onto {} more photos (crops kept)", n))
                    }
                    (n, failed) => self.toast(&gettext!(
                        "Edits pasted onto {} more photos, {} failed: {}",
                        n,
                        failed.len(),
                        failed.join("; ")
                    )),
                }
                self.roll_sent = None;
                self.render_roll();
            }
            Msg::Fullscreen => self.ui.window.set_fullscreened(!self.ui.window.is_fullscreen()),
            Msg::BatchTo(dir) => {
                let left: Vec<PathBuf> = self
                    .roll
                    .iter()
                    .filter(|it| self.ui.filmstrip.shows(it.rating, it.label))
                    .map(|it| it.path.clone())
                    .rev()
                    .collect();
                let total = left.len();
                let back = self.path.clone();
                self.batch = Some(Batch { left, dir, total, failed: Vec::new(), back, waiting: None, stop: false });
                self.ui.export_photo.set_busy(true);
                self.batch_next(&sender);
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
                        let thumb = display_color::to_monitor(rgba8(&reference.pixels));
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
                    .filters(&file_filters(&label, &[format.ext()]))
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
            Msg::ExportPhoto(_) if self.exporting.is_some() => {
                self.ui.export_photo.dialog.present(Some(&self.ui.window))
            }
            Msg::ExportPhoto(format) => {
                let Some((crop, _)) = self.crop_region().zip(self.path.as_ref()).filter(|_| !self.show_chart) else {
                    return self.toast(&gettext("Open a photo to export it"));
                };
                let ui = &self.ui.export_photo;
                ui.set_format(format.unwrap_or_else(|| export_ui::remembered().0));
                let shown = self.roll.iter().filter(|it| self.ui.filmstrip.shows(it.rating, it.label)).count();
                ui.set_roll(shown);
                let (w, h) = Decoded::fit_size(&crop, u32::MAX);
                ui.set_size(w, h);
                ui.set_headroom(self.headroom);
                ui.dialog.present(Some(&self.ui.window));
                self.export_frame_request();
            }
            Msg::ExportSettings => {
                let ui = &self.ui.export_photo;
                ui.set_format(ui.format());
                // The frame holds HDR gains or not: rendered again only when that changes.
                match &self.export_frame {
                    Some(f) if f.hdr() == ui.format().hdr() => self.export_estimate(&sender),
                    _ => self.export_frame_request(),
                }
            }
            Msg::HdrPreview(on) => {
                self.hdr_preview = on;
                let _ = settings::set_value(settings::HDR_PREVIEW, if on { "1" } else { "0" });
                self.render();
            }
            Msg::Headroom(stops) => {
                self.headroom = stops;
                self.render();
                // Saved once the slider rests, not per tick.
                let s = sender.input_sender().clone();
                glib::timeout_add_local_once(std::time::Duration::from_millis(500), move || {
                    s.emit(Msg::HeadroomSave(stops))
                });
            }
            Msg::HeadroomSave(stops) if stops == self.headroom => {
                let _ = settings::set_value(settings::HDR_HEADROOM, &format!("{stops:.2}"));
            }
            Msg::HeadroomSave(_) => {}
            Msg::ExportEstimateRun(generation, w, h) if generation == self.export_gen => {
                let ui = &self.ui.export_photo;
                // A frame for the other kind (SDR or HDR) is stale: the right one is on its way.
                let Some(frame) = self.export_frame.clone().filter(|f| f.hdr() == ui.format().hdr()) else {
                    return;
                };
                let (format, quality, s) = (ui.format(), ui.quality(), sender.input_sender().clone());
                std::thread::spawn(move || {
                    let scale = (w as f64 * h as f64) / (frame.w as f64 * frame.h as f64);
                    let n = export::encoded_size(&frame, format, quality) as f64 * scale;
                    s.emit(Msg::ExportEstimate(generation, n as u64));
                });
            }
            Msg::ExportEstimateRun(..) => {}
            Msg::ExportEstimate(generation, n) if generation == self.export_gen => {
                self.ui.export_photo.set_estimate(Some(gettext!("about {}", glib::format_size(n))))
            }
            Msg::ExportEstimate(..) => {}
            Msg::ExportGo => {
                if let Some(b) = self.batch.as_mut() {
                    b.stop = true;
                }
                if let Some(cancel) = &self.exporting {
                    return cancel.store(true, Ordering::Relaxed);
                }
                if self.batch.is_some() {
                    // Stopped between two photos.
                    return self.batch_stop(&sender);
                }
                if self.ui.export_photo.all() {
                    let dialog = gtk::FileDialog::builder().title(gettext("Export Photos To")).modal(true).build();
                    dialog.set_initial_folder(
                        self.path.as_ref().and_then(|p| p.parent()).map(gio::File::for_path).as_ref(),
                    );
                    let s = sender.clone();
                    dialog.select_folder(Some(&self.ui.window), gio::Cancellable::NONE, move |r| {
                        if let Some(p) = r.ok().and_then(|f| f.path()) {
                            s.input(Msg::BatchTo(p));
                        }
                    });
                    return;
                }
                let (Some(path), format) = (&self.path, self.ui.export_photo.format()) else { return };
                let stem = stem_label(path).unwrap_or_default();
                let dialog = gtk::FileDialog::builder()
                    .title(gettext("Export Photo"))
                    // Never the original's own name: a JPEG source sits in the same folder.
                    .initial_name(edit_name(&stem, format.ext(), &export_suffix()))
                    .filters(&file_filters(&format.ext().to_uppercase(), &[format.ext()]))
                    .modal(true)
                    .build();
                dialog.set_initial_folder(path.parent().map(gio::File::for_path).as_ref());
                let s = sender.clone();
                dialog.save(Some(&self.ui.window), gio::Cancellable::NONE, move |r| {
                    if let Some(p) = r.ok().and_then(|f| f.path()) {
                        s.input(Msg::ExportPhotoTo(p));
                    }
                });
            }
            Msg::ExportPhotoTo(path) => {
                self.export_to(path);
            }
            // Closing the dialog stops the export.
            Msg::ExportClosed => {
                if let Some(cancel) = &self.exporting {
                    cancel.store(true, Ordering::Relaxed);
                }
                if let Some(b) = self.batch.as_mut() {
                    b.stop = true;
                    if self.exporting.is_none() {
                        self.batch_stop(&sender);
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
            Msg::Off(part, off) => {
                self.off.retain(|&p| p != part);
                if off {
                    self.off.push(part);
                    self.off.sort();
                }
                match part {
                    Off::Grade(_) => self.render(),
                    Off::Looks => self.layers_changed(Layers::Look, false, &sender),
                    Off::Develop(_) if self.camera().is_some() => self.develop_changed(&sender),
                    Off::Develop(_) => {}
                    Off::Crop => self.crop_changed(&sender),
                }
                self.schedule_save();
            }
            Msg::Browse(on) => {
                self.browsing = on;
                // The applied LUTs go with the browser, so a LUT clicked by mistake goes without leaving it.
                let s = &self.ui.sidebar;
                if on && s.applied_dock.child().is_none() {
                    s.look_box.remove(&s.applied);
                    s.applied_dock.set_child(Some(&s.applied));
                    // Out of the section, the cards carry its switched-off dimming themselves.
                    if s.look_box.has_css_class("bypassed") {
                        s.applied.add_css_class("bypassed");
                    }
                } else if !on && s.applied_dock.child().is_some() {
                    s.applied_dock.set_child(None::<&gtk::Widget>);
                    s.applied.remove_css_class("bypassed");
                    s.look_box.prepend(&s.applied);
                }
                self.ui.sidebar.side_stack.set_visible_child_name(if on { "browser" } else { "sections" });
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
                sender.input(Msg::Preview(over));
            }
            Msg::HoverSettled(..) => {}
            Msg::Commit(i) => {
                let path = self.luts.get(i).map(|l| &l.path);
                // A double-click activates twice: the second adds nothing, whether the first is loading or in.
                if self.flash_new_look
                    || path.is_some_and(|p| self.looks.last().and_then(|l| l.path.as_ref()) == Some(p))
                {
                    return;
                }
                self.flash_new_look = true;
                match self.candidate.take().filter(|c| c.0 == i) {
                    Some((_, lut)) => {
                        let path = self.luts.get(i).map(|l| l.path.clone());
                        self.push_lut(Layers::Look, lut, path, None, &sender)
                    }
                    None => {
                        if let Some(entry) = self.luts.get(i) {
                            Self::load_layer(Layers::Look, entry.path.clone(), None, &sender);
                        }
                    }
                }
                // Added: no longer a preview. The card stays selected for Open in LUT Lab.
                self.drop_candidate();
            }
            Msg::Escape => {
                if self.ui.sidebar.develop.picker.is_active() {
                    self.ui.sidebar.develop.picker.set_active(false);
                } else if self.editing.is_some() {
                    sender.input(Msg::CropCancel);
                } else if self.candidate.is_some() || self.preview_req.is_some() {
                    self.clear_selection();
                } else if self.browsing {
                    sender.input(Msg::Browse(false));
                }
            }
            Msg::Profile(profile) => {
                self.profile = profile;
                self.schedule_save();
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
                    Some(entry) => Self::load_layer(which, entry.path.clone(), None, &sender),
                    None => self.toast(&gettext("Select a LUT in the browser first")),
                },
                LabAdd::File => {
                    let dialog = gtk::FileDialog::builder()
                        .title(gettext("Add LUT"))
                        .filters(&file_filters(&gettext("LUTs (.cube, .vlt, HALD .png)"), &LUT_EXTENSIONS))
                        .modal(true)
                        .build();
                    dialog.set_initial_folder(start_in(settings::LUT_FOLDER).as_ref());
                    let s = sender.clone();
                    dialog.open(Some(&self.ui.window), gio::Cancellable::NONE, move |r| {
                        if let Some(p) = r.ok().and_then(|f| f.path()) {
                            Self::load_layer(which, p, None, &s);
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
            Msg::LayerLoaded(which, p, known, Ok(lut)) => self.push_lut(which, Arc::new(lut), Some(p), known, &sender),
            Msg::LayerLoaded(_, p, _, Err(e)) => {
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
                // The header's Export saves the photo or the grade's LUT; LUT Lab has its own export.
                self.ui.export.set_visible(!matches!(page, Page::Lab));
                // Zoom is a Grade canvas view; the other pages show the whole photo.
                if !matches!(page, Page::Grade) {
                    self.set_zoom(None);
                }
                self.refresh();
            }
            Msg::Gpu(done) => self.on_gpu(done, &sender),
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
                    // A ratio chosen is a crop to place: its frame and handles come up.
                    self.ui.crop.edit.set_active(true);
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
        Some(crop::region(&if self.off.contains(&Off::Crop) { Crop::default() } else { self.crop }, w, h))
    }

    /// What the preview shows: the whole straightened frame while editing
    /// the crop, otherwise the crop.
    fn shown_region(&self) -> Option<Region> {
        if self.editing.is_none() {
            return self.crop_region();
        }
        let (w, h) = self.size()?;
        Some(crop::region(&crop::frame(self.crop), w, h))
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
        // Past 100% the canvas upsamples a 1:1 develop, so detail radii keep their full-resolution meaning (as darktable does).
        let k = (r.size[0] / w as f32).min(r.size[1] / h as f32).min(1.0);
        let (w, h) = (((w as f32 * k).round() as u32).max(1), ((h as f32 * k).round() as u32).max(1));
        self.view_gen += 1;
        let (s, generation, profile) = (self.input.clone(), self.view_gen, self.profile);
        if d.is_raw() {
            let ((width, height), develop) = (self.draft_size((w, h)), self.applied_develop());
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

    fn on_gpu(&mut self, done: Done, sender: &ComponentSender<Self>) {
        match done {
            Done::Failed(e) => {
                // The worker is gone: an export in progress never reports back.
                if self.exporting.take().is_some() {
                    self.ui.export_photo.set_busy(false);
                }
                self.toast(&gettext!("GPU unavailable: {}", e))
            }
            Done::Graded { width, height, rgba, hdr, scopes: Scopes { histogram, waveform, vectorscope } } => {
                let shown = match hdr {
                    Some(hdr) => hdr_texture(width, height, hdr),
                    None => texture(width, height, rgba).upcast(),
                };
                self.ui.grade_pic.set_paintable(Some(&shown));
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
            Done::ExportProgress(f) => {
                // A batch shows its own progress: the photos done plus this one's part.
                let f = self.batch.as_ref().map_or(f, |b| ((b.total - b.left.len() - 1) as f32 + f) / b.total as f32);
                self.ui.export_photo.set_progress(f);
            }
            Done::ExportFrame(frame) => {
                self.export_frame = Some(Arc::new(frame));
                self.export_estimate(sender);
            }
            Done::PhotoExported(r) if self.batch.is_some() => {
                self.exporting = None;
                match r {
                    Ok(Some(_)) => {}
                    Ok(None) => return self.batch_stop(sender),
                    Err(e) => {
                        let name = self.path.as_deref().map(file_label).unwrap_or_default();
                        self.batch.as_mut().expect("guarded").failed.push(format!("{name}: {e}"));
                    }
                }
                self.batch_next(sender);
            }
            Done::PhotoExported(r) => {
                self.exporting = None;
                self.ui.export_photo.set_busy(false);
                match r {
                    Ok(Some(p)) => {
                        self.ui.export_photo.dialog.close();
                        self.toast(&gettext!("Photo saved: {}", file_label(&p)));
                    }
                    Ok(None) => self.toast(&gettext("Export stopped")),
                    Err(e) => self.toast(&gettext!("Export failed: {}", e)),
                }
            }
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
            Done::Developed { view: false, profile, photo: Some(photo), cost, .. } => {
                // Smoothed: one frame can be slow for reasons that pass (a cache miss, the compositor).
                self.frame_cost = if self.frame_cost > 0.0 { 0.7 * self.frame_cost + 0.3 * cost } else { cost };
                if let Some(r) = self.shown_region() {
                    let (w, h) = Decoded::fit_size(&r, PREVIEW_MAX);
                    self.auto_draft = auto_draft(self.frame_cost, w * h);
                }
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
            Done::AiProgress(g, f) if g == self.load_gen && self.develop.detail.nr_ai => {
                let ui = &self.ui.sidebar.develop;
                ui.ai_bar.set_fraction(f as f64);
                ui.ai_busy.set_visible(true);
                ui.ai_msg.set_visible(false);
            }
            Done::AiReady(g) if g == self.load_gen => {
                self.ui.sidebar.develop.ai_busy.set_visible(false);
                self.input.emit(Msg::AiReady);
            }
            Done::AiFailed(g, e) if g == self.load_gen => {
                let ui = &self.ui.sidebar.develop;
                ui.ai_busy.set_visible(false);
                ui.ai_msg.set_label(&gettext!("AI noise reduction unavailable: {}", e));
                ui.ai_msg.set_visible(self.develop.detail.nr_ai);
            }
            Done::AiProgress(..) | Done::AiReady(_) | Done::AiFailed(..) => {}
            Done::Sampled(c) => match self.camera().and_then(|cam| cam.neutral_for(c)) {
                Some(kt) => self.input.emit(Msg::Develop(DevelopEdit::Picked(kt))),
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

/// The Grade tab's line naming the exported LUT's input, with a link to the Photo tab.
fn profile_link_markup(p: Profile) -> String {
    gettext!("Exported LUT input: <b>{}</b>. <a href=\"photo\">Change in Photo</a>", glib::markup_escape_text(p.name()))
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

/// Menu of export formats for action `action` (`win.<action>::<kind>-<size>`);
/// the `header` one also has the photo formats and the HALD identity.
fn export_menu(action: &str, header: bool) -> gio::Menu {
    let item =
        |m: &gio::Menu, label: &str, target: &str| m.append(Some(label), Some(&format!("win.{action}::{target}")));
    let menu = gio::Menu::new();
    if header {
        let photo = gio::Menu::new();
        for f in PhotoFormat::ALL {
            photo.append(Some(&(export_ui::format_label(f) + "…")), Some(&format!("win.export-photo::{}", f.tag())));
        }
        menu.append_section(Some(gettext("Photo").as_str()), &photo);
    }
    // .cube is the universal 3D LUT format; the sizes differ only in precision.
    let cube = gio::Menu::new();
    item(&cube, &gettext("33³ .cube · cameras and monitors"), "cube-33");
    item(&cube, &gettext("65³ .cube · grading and editing"), "cube-65");
    item(&cube, &gettext("129³ .cube · maximum precision"), "cube-129");
    menu.append_section(Some(gettext("LUT · .cube").as_str()), &cube);
    let other = gio::Menu::new();
    item(&other, &gettext("Panasonic VLT · 17³ .vlt"), "vlt-17");
    item(&other, &gettext("HALD CLUT · 64³ .png"), "hald-64");
    menu.append_section(Some(gettext("LUT · other formats").as_str()), &other);
    if header {
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
        (gettext("Undo"), "<Control>z"),
        (gettext("Redo"), "<Control><Shift>z"),
        (gettext("Copy edits"), "<Control><Shift>c"),
        (gettext("Paste edits"), "<Control><Shift>v"),
        (gettext("Stars"), "1"),
        (gettext("No stars"), "0"),
        (gettext("Colour label: red, yellow, green, blue"), "6"),
        (gettext("Fullscreen"), "F11"),
        (gettext("Export photo"), "<Control>e"),
        (gettext("Export LUT · 33³ .cube"), "<Control><Shift>e"),
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
        (gettext("Photo view"), "<Alt>1"),
        (gettext("Grade view"), "<Alt>2"),
        (gettext("Metadata view"), "<Alt>3"),
        (gettext("LUT Lab view"), "<Alt>4"),
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

/// Installed: `<prefix>/share/rawvibes/lensfun.json`; dev: `make lensfun` puts it in `target/`.
fn lens_db_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.parent()?.join("share/rawvibes/lensfun.json")))
        .filter(|p| p.exists())
        .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../target/lensfun.json")))
}

/// Installed: `<prefix>/bin/rawvibes` → `<prefix>/share/locale`; dev: `make` puts catalogs in `target/locale`.
fn locale_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.parent()?.join("share/locale")))
        .filter(|d| d.join("it/LC_MESSAGES/rawvibes.mo").exists())
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
    /// Every section's eye toggle, to show a photo's switched-off sections.
    static EYES: std::cell::RefCell<Vec<(Off, gtk::ToggleButton)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The interface's translation of `msgid`, or `msgid` itself.
pub fn gettext(msgid: &str) -> String {
    let id = CString::new(msgid).expect("message ids have no NUL");
    // SAFETY: gettext returns a NUL-terminated static translation, or `id`, which outlives the copy.
    unsafe { CStr::from_ptr(gettext_sys::gettext(id.as_ptr())) }.to_string_lossy().into_owned()
}

/// Points gettext at the `rawvibes` catalogs in `dir`. gtk_init calls setlocale;
/// a missing catalog just leaves the English source strings.
fn bind_text_domain(dir: &Path) {
    let domain = c"rawvibes";
    let Ok(dir) = CString::new(dir.as_os_str().as_encoded_bytes()) else { return };
    // SAFETY: plain C calls with NUL-terminated strings that gettext copies.
    unsafe {
        gettext_sys::bindtextdomain(domain.as_ptr(), dir.as_ptr());
        gettext_sys::bind_textdomain_codeset(domain.as_ptr(), c"UTF-8".as_ptr());
        gettext_sys::textdomain(domain.as_ptr());
    }
}

fn main() {
    bind_text_domain(&locale_dir());
    // GTK 4.24 talks to the compositor's colour management (wp_color_manager_v1) only
    // when asked: what lets the HDR preview reach an HDR screen as HDR.
    #[cfg(target_os = "linux")]
    {
        let debug = std::env::var("GDK_DEBUG").unwrap_or_default();
        if !debug.split([',', ':', ' ']).any(|f| f == "color-mgmt") {
            let flags = if debug.is_empty() { "color-mgmt".to_string() } else { format!("{debug},color-mgmt") };
            // SAFETY: first thing in main, before GTK or any other thread starts.
            unsafe { std::env::set_var("GDK_DEBUG", flags) };
        }
    }
    // HANDLES_OPEN: files from the command line or a file manager ("Open With")
    // arrive here, also when RawVibes is already running.
    let app = adw::Application::builder().application_id(APP_ID).flags(gio::ApplicationFlags::HANDLES_OPEN).build();
    app.connect_open(|app, files, _| {
        let paths: Vec<PathBuf> = files.iter().filter_map(|f| f.path()).collect();
        if !paths.is_empty() {
            OPEN.with(|o| o.get().map(|s| s.emit(Msg::RollAddPaths(paths, None))));
        }
        app.activate();
    });
    // When RawVibes is already running this process only forwards the files,
    // and RelmApp::run then blocks on one last main-loop iteration: keep waking
    // the loop until we turn out to be the primary instance (startup).
    static STARTED: AtomicBool = AtomicBool::new(false);
    app.connect_startup(|_| {
        STARTED.store(true, Ordering::Relaxed);
        let css = CSS.replace("CANVAS_MARGIN", &CANVAS_MARGIN.to_string());
        relm4::set_global_css(&format!("{css}{}", band_css()));
        // The relm4-icons that build.rs bundled, as a resource the icon theme searches.
        let icons = gio::Resource::from_data(&glib::Bytes::from_static(icon_names::GRESOURCE_BYTES));
        icons.iter().for_each(gio::resources_register);
        // Installed, our own icons are in the hicolor theme; a dev run finds them in the source tree.
        if let Some(display) = gdk::Display::default() {
            let theme = gtk::IconTheme::for_display(&display);
            theme.add_resource_path(icon_names::RESOURCE_PREFIX);
            theme.add_search_path(concat!(env!("CARGO_MANIFEST_DIR"), "/../data/icons"));
        }
    });
    std::thread::spawn(|| {
        while !STARTED.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(100));
            glib::MainContext::default().wakeup();
        }
    });
    RelmApp::from_app(app).with_args(std::env::args().collect()).run::<App>(());
}

/// `<stem><suffix>.<ext>` in `dir`, or `<stem>-2<suffix>.<ext>` and on: never an existing file.
fn free_path(dir: &std::path::Path, stem: &str, ext: &str, suffix: &str) -> PathBuf {
    (1..)
        .map(|n| dir.join(edit_name(&if n == 1 { stem.to_string() } else { format!("{stem}-{n}") }, ext, suffix)))
        .find(|p| !p.exists())
        .expect("unbounded")
}

/// File dialog for photos and RAWs, starting where the last photo was opened.
fn photo_dialog(title: &str) -> gtk::FileDialog {
    // add_suffix matches case-insensitively.
    let pats: Vec<&str> = RASTER_EXTS.iter().chain(RAW_EXTS).copied().collect();
    let dialog = gtk::FileDialog::builder()
        .title(title)
        .filters(&file_filters(&gettext("Photos and RAWs"), &pats))
        .modal(true)
        .build();
    dialog.set_initial_folder(start_in(settings::PHOTO_FOLDER).as_ref());
    dialog
}

fn preset_dialog(title: &str) -> gtk::FileDialog {
    let filters = file_filters(&gettext("RawVibes presets"), &[preset::EXT]);
    gtk::FileDialog::builder().title(title).filters(&filters).modal(true).build()
}

fn no_photo_to_match() -> String {
    gettext("Open a photo first: the match is computed on it")
}

/// Display-encoded float pixels as RGBA8, for thumbnails.
fn rgba8(pixels: &[[f32; 4]]) -> Vec<u8> {
    pixels.iter().flat_map(|p| p.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A paste brings the edits but leaves the photo's crop, stars and label.
    #[test]
    fn paste_keeps_crop_and_marks() {
        let mut clip = Sidecar { rating: 5, label: Label::Red, off: vec![Off::Crop, Off::Looks], ..Default::default() };
        clip.develop.exposure = 1.0;
        clip.crop.angle = 3.0;
        let target = Sidecar {
            rating: 2,
            label: Label::Blue,
            meta: Meta { title: "B".into(), ..Default::default() },
            ..Default::default()
        };
        clip.meta.title = "A".into();
        let p = pasted(&clip, &target);
        assert_eq!((p.develop.exposure, p.crop, p.rating, p.label), (1.0, target.crop, 2, Label::Blue));
        assert_eq!(p.off, [Off::Looks]);
        assert_eq!(p.meta, target.meta);
    }

    #[test]
    fn suffix_is_sanitized() {
        assert_eq!(edit_name("IMG_1", "jpg", ""), "IMG_1_edit.jpg");
        assert_eq!(edit_name("IMG_1", "jpg", "  "), "IMG_1_edit.jpg");
        assert_eq!(edit_name("IMG_1", "jpg", "../x"), "IMG_1..x.jpg");
        assert_eq!(edit_name("IMG_1", "jpg", "-web"), "IMG_1-web.jpg");
    }

    /// The catalogs `make` builds are found and read (needs the it_IT.UTF-8 locale).
    #[cfg(target_os = "linux")]
    #[test]
    fn italian_catalog_translates() {
        // SAFETY: LC_ALL = 6 on glibc; the only test that touches the locale.
        if unsafe { gettext_sys::setlocale(6, c"it_IT.UTF-8".as_ptr()) }.is_null() {
            return eprintln!("no it_IT.UTF-8 locale, skipped");
        }
        bind_text_domain(&locale_dir());
        assert_eq!(gettext("Open Photo"), "Apri foto");
    }

    /// A frame within budget renders at full size; four times over budget, at half
    /// size (a quarter of the pixels); hopelessly slow GPUs stop at the floor.
    #[test]
    fn auto_draft_fits_the_frame_budget() {
        let px = 2048 * 1365;
        let cost = |secs: f32| secs / px as f32;
        assert_eq!(auto_draft(cost(FRAME_BUDGET * 0.5), px), 1.0);
        assert!((auto_draft(cost(FRAME_BUDGET * 4.0), px) - 0.5).abs() < 1e-4);
        assert_eq!(auto_draft(cost(1.0), px), 0.4);
    }
}
