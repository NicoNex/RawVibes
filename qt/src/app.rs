//! The editor: the GTK UI's `App` ported to a toolkit-free controller. QML sends commands
//! (`command.rs`), this updates the state, and `view.rs` turns it into the JSON QML shows.

pub mod command;
mod flows;

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use rawvibes_core::chart::test_chart;
use rawvibes_core::crop::{self, Crop, PRESETS, Ratio};
use rawvibes_core::develop::{Detail, Develop};
use rawvibes_core::export::{self, PhotoFormat};
use rawvibes_core::gpu::{self, Done, Job};
use rawvibes_core::grade::{Curve, Grade, MASTER, Params, Stage, neutral_curve};
use rawvibes_core::lab::{Blend, Layer, Source};
use rawvibes_core::lensdb;
use rawvibes_core::lut::{self, Format, Lut3d, LutEntry, MAX_LIBRARY};
use rawvibes_core::makernote::CameraLut;
use rawvibes_core::matching::Match;
use rawvibes_core::meta::RawMetadata;
use rawvibes_core::photo::{Decoded, Photo};
use rawvibes_core::preset::{self, Preset};
use rawvibes_core::profiles::Profile;
use rawvibes_core::raw::RawData;
use rawvibes_core::sidecar::{Label, LensChoice, LookRef, Meta, Off, Sidecar};
use rawvibes_core::view::{Region, Zoom};

pub use command::command;

use crate::bridge::Tx;
use crate::gettext;
use crate::i18n::gettext;
use crate::settings;
use crate::timers::Timers;

/// Most photos in the filmstrip, and the long side of their small copies.
pub const MAX_ROLL: usize = 12;
pub const ROLL_PX: u32 = 720;

pub type Field = fn(&mut Params) -> &mut f32;
/// One of the detail tools' amounts.
pub type DetailField = fn(&mut Detail) -> &mut f32;

/// What a LUT export writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum What {
    Grade,
    /// The neutral LUT (e.g. a HALD to process in another editor).
    Identity,
    Lab,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Grade,
    Lab,
}

#[derive(Clone, Debug)]
pub enum LabKind {
    Lut(Arc<Lut3d>),
    Grade,
    Convert(Profile),
}

/// The two layer stacks: the LUT Lab's and the LUTs applied after the grade.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layers {
    Lab,
    Look,
}

#[derive(Clone, Debug)]
pub struct LabLayer {
    pub kind: LabKind,
    pub intensity: f32,
    /// Signal the LUT expects (unused for grade and conversions).
    pub input: Profile,
    /// Off layers keep their settings but are skipped everywhere.
    pub on: bool,
    /// The LUT's file, when it came from one (saved in the sidecar).
    pub path: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy)]
pub enum LabAdd {
    Selected,
    File,
    Grade,
    Convert,
}

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

/// What the lens profile row says.
#[derive(Clone, Debug, PartialEq)]
pub enum LensStatus {
    Auto(String),
    Chosen(String),
    NeedsFocal(String),
    NotFound,
    Off,
    NoDatabase,
}

#[derive(Clone, Copy, Debug)]
pub enum ZoomCmd {
    Fit,
    Actual,
    In,
    Out,
    /// Wheel/touchpad scroll of `dy` steps about a point.
    Scroll { dy: f64, at: [f32; 2] },
    /// Drag by `d` with mouse `button` (1 primary, 2 middle).
    Pan { d: [f32; 2], button: u32 },
    /// Double-click: fit ↔ 100% about a point.
    Toggle([f32; 2]),
}

/// What an "As in camera" button sets.
#[derive(Debug, Clone, Copy)]
pub enum CameraPart {
    /// Reference match: the whole match to the camera JPEG.
    Match,
    /// As-shot white balance and exposure, then the tone curve matched to the camera JPEG.
    All,
}

/// A change in the Develop group.
#[derive(Debug, Clone, Copy)]
pub enum DevelopEdit {
    AsShot,
    /// Dropdown index: 0 As shot, then `WB_PRESETS`, then Custom.
    Preset(u32),
    Kelvin(f32),
    Tint(f32),
    Exposure(f32),
    CameraExposure,
    Detail(DetailField, f32),
    CaCorrect(bool),
    LensTca(bool),
    /// Noise reduction method: AI (true) or classic.
    NrAi(bool),
    Pick(bool),
    Picked([f32; 2]),
}

/// How the RAW preview trades resolution for fluidity while a development control moves.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PreviewQuality {
    Auto,
    Fast,
    Balanced,
    Best,
}

impl PreviewQuality {
    pub const ALL: [PreviewQuality; 4] =
        [PreviewQuality::Auto, PreviewQuality::Fast, PreviewQuality::Balanced, PreviewQuality::Best];

    pub fn key(self) -> &'static str {
        match self {
            PreviewQuality::Auto => "auto",
            PreviewQuality::Fast => "fast",
            PreviewQuality::Balanced => "balanced",
            PreviewQuality::Best => "best",
        }
    }

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

pub const SAVE_SETTLE: Duration = Duration::from_millis(1000);
pub const WRITE_SETTLE: Duration = Duration::from_millis(250);
/// GPU time a draft frame may take under Automatic preview quality.
const FRAME_BUDGET: f32 = 0.012;
pub const DEVELOP_SETTLE: Duration = Duration::from_millis(150);
pub const HOVER_REST: Duration = Duration::from_millis(120);
pub const ROLL_SETTLE: Duration = Duration::from_millis(150);

/// The linear draft scale whose frame fits `FRAME_BUDGET`; never below 0.4.
fn auto_draft(cost: f32, pixels: u32) -> f32 {
    (FRAME_BUDGET / (cost * pixels as f32)).sqrt().clamp(0.4, 1.0)
}

/// A photo's edits and its look layers (those without a file too).
pub type Edits = (Sidecar, Vec<LabLayer>);
const HISTORY_MAX: usize = 100;

#[derive(Debug)]
pub enum Msg {
    Open,
    Loaded(u64, PathBuf, Result<Decoded, String>),
    Develop(DevelopEdit),
    AsInCamera(CameraPart),
    CameraJpeg(PathBuf, Result<Arc<Photo>, String>),
    CameraCurve(PathBuf, Option<Arc<Photo>>, Option<Curve>),
    CameraLut(PathBuf, CameraLut, Option<PathBuf>),
    CameraLutFind(PathBuf, CameraLut),
    CameraLutAdd(PathBuf, CameraLut, PathBuf),
    /// The user found the camera LUT's file: remembered by name, then used.
    CameraLutFound(PathBuf, CameraLut, PathBuf),
    AsInCameraOnOpen(bool),
    SaveSidecar(u64),
    WriteSidecar(u64),
    Close,
    SidecarLooks(u64, Vec<(LookRef, Result<Arc<Lut3d>, String>)>),
    CanvasClick([f32; 2]),
    DevelopSettled(u64),
    AiReady,
    Quality(u32),
    RawRead(u64, PathBuf, Result<Arc<RawData>, String>),
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
    Reload,
    Off(Off, bool),
    Browse(bool),
    Preview(Option<usize>),
    PreviewLoaded(usize, Result<Arc<Lut3d>, String>),
    Hover(Option<usize>),
    HoverSettled(u64, Option<usize>),
    Commit(usize),
    Escape,
    Page(Page),
    Tab(String),
    LayerAdd(Layers, LabAdd),
    OpenInLab,
    LayerLoaded(Layers, PathBuf, Option<(Profile, f32)>, Result<Lut3d, String>),
    LayerRemove(Layers, usize),
    LayerMove(Layers, usize, bool),
    LayerIntensity(Layers, usize, f64),
    LayerProfile(Layers, usize, Profile),
    LabInput(Profile),
    LabBlend(Blend),
    LookBlend(Blend),
    LayerOn(Layers, usize, bool),
    FolderChanged,
    FolderSettled(u64),
    Chart(bool),
    ChartReady(Profile, Arc<Photo>),
    Profile(Profile),
    SelectLut(usize),
    MixerMono(bool),
    MixerFree(bool),
    Mixer(usize, f64),
    MatchChoose,
    MatchReference(PathBuf),
    MatchLoaded(PathBuf, Result<Arc<Photo>, String>),
    MatchClear,
    MatchAmount(f64),
    Overlay(bool),
    SkinLine(bool),
    PresetsRefresh,
    PresetSave,
    PresetSaveAs(String),
    PresetWrite(Box<Preset>, bool, bool),
    PresetApply(PathBuf),
    PresetDelete(PathBuf),
    PresetRestore(PathBuf, String),
    PresetImport,
    PresetImported(PathBuf),
    PresetExport,
    PresetExportTo(PathBuf),
    ExportPhoto(Option<PhotoFormat>),
    ExportSettings(PhotoFormat, u8, bool),
    HdrPreview(bool),
    Headroom(f32),
    HeadroomSave(f32),
    ExportEstimateRun(u64, u32, u32),
    ExportEstimate(u64, u64),
    ExportGo,
    ExportPhotoTo(PathBuf),
    ExportClosed,
    RestoreGrade(Box<Grade>),
    RestoreEdits(Box<Edits>),
    MetaEdit(MetaField, String),
    SourceMeta(u64, PathBuf, Option<Arc<RawMetadata>>),
    LensSearch(String),
    LensPick(Option<(String, String)>),
    LensManual(f32, f32),
    LensDb(Option<Arc<lensdb::Db>>),
    Refine(bool),
    RollAddPaths(Vec<PathBuf>, Option<PathBuf>),
    MergeHdr,
    MergeHdrRun(Vec<PathBuf>),
    MergeHdrDone(Result<PathBuf, String>),
    RollLoaded(PathBuf, Result<(Decoded, Profile, Arc<Photo>), String>),
    RollEncoded(Profile, Vec<(PathBuf, Arc<Photo>)>),
    RollTick(u64),
    RollSelect(usize),
    RollRemove(usize),
    RollFilter(u32),
    Grid(bool),
    Gpu(Done),
    Zoom(ZoomCmd),
    CanvasSize([f32; 2]),
    ViewReady(u64, Profile, Arc<Photo>),
    CropEdit(bool),
    CropCancel,
    CropRatio(usize),
    CropSwap,
    CropAngle(f32),
    CropRect([f32; 4]),
    CropReset,
    Undo,
    Redo,
    Rate(u8, bool),
    Mark(Label),
    CopyEdits,
    PasteEdits,
    PasteEditsAll,
    BatchTo(PathBuf),
    /// A toast's button was pressed.
    ToastAction(i32),
    /// Preferences.
    Background(String),
    MetaPolicy(String),
    ExportSuffix(String),
    AuthorDefault(String, String),
}

/// A batch export in progress: photos still to export (last first), where to, and how it went.
pub struct Batch {
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

/// What `find_lens` found for the open RAW, and what the Lens section shows of it.
pub struct LensView {
    profile: Option<rawvibes_core::lens::Profile>,
    lens: Option<(String, String)>,
    status: LensStatus,
    focal: f32,
    aperture: f32,
    asked: bool,
}

/// A photo of the filmstrip: its file, a small decoded copy once read, and that copy encoded.
pub struct RollItem {
    pub path: PathBuf,
    /// The photo's own grade (its sidecar's), for its filmstrip frame.
    pub grade: Grade,
    /// Its look layers when last left this session: the sidecar keeps only those with a file.
    pub looks: Vec<LabLayer>,
    pub rating: u8,
    pub label: Label,
    pub small: Option<Decoded>,
    pub encoded: Option<(Profile, Arc<Photo>)>,
    /// Bumped with each frame the GPU renders for it (QML reloads image://rv/roll/<i>/<gen>).
    pub frame: u64,
}

/// What the GTK UI kept in its widgets: plain state QML draws from.
#[derive(Default)]
pub struct UiState {
    pub tab: String,
    pub info: String,
    /// "empty", "loading" or "image".
    pub canvas: &'static str,
    pub photo_gen: u64,
    pub scopes_gen: u64,
    pub lab_gen: u64,
    pub match_thumb: u64,
    pub thumbs: Vec<u64>,
    pub picker: bool,
    pub skin: bool,
    pub grid: bool,
    pub roll_filter: u32,
    /// "empty", "busy" or "set".
    pub matching: &'static str,
    pub match_name: String,
    pub ai_busy: bool,
    pub ai_fraction: f32,
    pub ai_msg: Option<String>,
    pub lens_status: Option<LensStatus>,
    pub lens_focal: f32,
    pub lens_aperture: f32,
    pub lens_asked: bool,
    pub lens_results: Vec<(String, String)>,
    pub export: ExportState,
    /// Index of the look just added from the browser, to flash once.
    pub flash_look: Option<usize>,
    pub presets: Vec<(String, PathBuf)>,
}

#[derive(Default)]
pub struct ExportState {
    pub open: bool,
    pub format: Option<PhotoFormat>,
    pub quality: u8,
    pub all: bool,
    pub roll: usize,
    pub size: (u32, u32),
    pub estimate: Option<String>,
    pub busy: bool,
    pub progress: f32,
    pub step: String,
}

pub struct App {
    pub grade: Grade,
    pub band: usize,
    pub compare: bool,
    pub split: f32,
    pub path: Option<PathBuf>,
    pub luts: Vec<LutEntry>,
    lut_paths: Arc<Vec<PathBuf>>,
    pub group_names: Vec<String>,
    pub group: Option<String>,
    pub query: String,
    pub page: Page,
    pub lab_layers: Vec<LabLayer>,
    pub looks: Vec<LabLayer>,
    pub off: Vec<Off>,
    pub meta: Meta,
    pub lens_profile: Option<LensChoice>,
    lens_db: Option<Arc<lensdb::Db>>,
    source_meta: Option<(PathBuf, Arc<RawMetadata>)>,
    pub browsing: bool,
    pub candidate: Option<(usize, Arc<Lut3d>)>,
    preview_req: Option<usize>,
    hover_tick: u64,
    flash_new_look: bool,
    base_key: Option<BaseKey>,
    gallery_stale: bool,
    pub lab_input: Profile,
    pub lab_blend: Blend,
    pub look_blend: Blend,
    /// Watches the LUT folder; `folder_changes` debounces its events.
    watcher: Option<notify::RecommendedWatcher>,
    folder_changes: u64,
    pub profile: Profile,
    decoded: Option<Decoded>,
    photo: Option<(Arc<Photo>, Profile)>,
    chart: Option<(Arc<Photo>, Profile)>,
    pub show_chart: bool,
    pub selected: Option<usize>,
    jobs: mpsc::Sender<Job>,
    gpu_thread: Option<std::thread::JoinHandle<()>>,
    exporting: Option<Arc<AtomicBool>>,
    export_frame: Option<Arc<export::Exported>>,
    export_gen: u64,
    pub hdr_preview: bool,
    pub headroom: f32,
    sent: Option<(Grade, f32)>,
    load_gen: u64,
    gallery_gen: u64,
    pub overlay: bool,
    pub roll: Vec<RollItem>,
    pub active: Option<usize>,
    roll_gen: u64,
    roll_sent: Option<Vec<Grade>>,
    roll_tick: u64,
    match_file: bool,
    match_gen: u64,
    pub crop: Crop,
    pub ratio: Ratio,
    /// While editing the crop: the crop, ratio and ratio row before editing (Escape restores them).
    pub editing: Option<(Crop, Ratio, u32)>,
    pub ratio_row: u32,
    pub zoom: Option<Zoom>,
    canvas: [f32; 2],
    view_gen: u64,
    encoding: bool,
    pub develop: Develop,
    pub quality: PreviewQuality,
    drafting: bool,
    frame_cost: f32,
    auto_draft: f32,
    pub refine: bool,
    develop_tick: u64,
    camera_match: Option<(PathBuf, Match)>,
    camera_curve: Option<(PathBuf, Curve)>,
    camera_jpeg: Option<(PathBuf, Arc<Photo>)>,
    camera_job: Option<PathBuf>,
    saved: Option<Sidecar>,
    save_tick: u64,
    looks_loading: bool,
    looks_gen: u64,
    save_failed: bool,
    encode_again: bool,
    encoded: Option<Region>,
    pub rating: u8,
    pub label: Label,
    history: Vec<Edits>,
    undo_at: usize,
    pub clipboard: Option<Edits>,
    batch: Option<Batch>,
    pub ui: UiState,
    tx: Tx,
    timers: Timers,
    /// Messages the update itself queued (GTK's `sender.input`), run before publishing.
    queue: VecDeque<Msg>,
    toasts: Vec<(String, String, i32)>,
    dialogs: Vec<(String, String)>,
    actions: HashMap<i32, Box<dyn Fn() -> Msg>>,
    next_action: i32,
    /// The state changed since QML last read it.
    dirty: bool,
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

impl App {
    pub fn new(tx: Tx) -> Self {
        let g = tx.clone();
        let (jobs, gpu_thread) = gpu::spawn(move |d| g.send(Msg::Gpu(d)));
        let (format, quality) = remembered_export();
        let mut app = App {
            grade: Grade::default(),
            band: 0,
            compare: false,
            split: 0.5,
            path: None,
            luts: Vec::new(),
            lut_paths: Arc::new(Vec::new()),
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
            watcher: None,
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
            hdr_preview: settings::value(settings::HDR_PREVIEW).as_deref() == Some("1"),
            headroom: headroom_setting(),
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
            crop: Crop::default(),
            ratio: Ratio::Free,
            editing: None,
            ratio_row: 0,
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
            ui: UiState {
                tab: "grade".into(),
                canvas: "empty",
                matching: "empty",
                export: ExportState { format: Some(format), quality, ..Default::default() },
                ..Default::default()
            },
            timers: Timers::new(tx.clone()),
            tx,
            queue: VecDeque::new(),
            toasts: Vec::new(),
            dialogs: Vec::new(),
            actions: HashMap::new(),
            next_action: 1,
            dirty: true,
        };
        // Pick up where the last session left: its filmstrip and open photo (files since moved are skipped).
        let roll: Vec<PathBuf> = settings::value(settings::ROLL)
            .unwrap_or_default()
            .lines()
            .map(PathBuf::from)
            .filter(|p| p.is_file())
            .collect();
        if !roll.is_empty() {
            let last = settings::value(settings::LAST_PHOTO).map(PathBuf::from);
            app.queue.push_back(Msg::RollAddPaths(roll, last));
        }
        if let Some(dir) = settings::folder(settings::LUT_FOLDER) {
            app.queue.push_back(Msg::Folder(dir));
        }
        app.queue.push_back(Msg::PresetsRefresh);
        // 3 MB of JSON: read off the main thread.
        let s = app.tx.clone();
        std::thread::spawn(move || s.send(Msg::LensDb(lensdb::Db::load(&lens_db_path()).ok().map(Arc::new))));
        app
    }

    /// Runs `msg` and whatever it queues.
    pub fn update(&mut self, msg: Msg) {
        self.queue.push_back(msg);
        while let Some(m) = self.queue.pop_front() {
            self.dispatch(m);
        }
    }

    /// The state for QML when it changed, and the toasts and dialogs to show.
    pub fn publish(&mut self) -> (Option<String>, Vec<(String, String, i32)>, Vec<(String, String)>) {
        let state = std::mem::take(&mut self.dirty).then(|| crate::view::state(self).to_string());
        (state, std::mem::take(&mut self.toasts), std::mem::take(&mut self.dialogs))
    }

    fn input(&mut self, msg: Msg) {
        self.queue.push_back(msg);
    }

    fn send(&mut self, job: Job) {
        if self.jobs.send(job).is_err() {
            self.toast(&gettext("GPU unavailable"));
        }
    }

    pub fn toast(&mut self, text: &str) {
        self.toasts.push((text.to_owned(), String::new(), 0));
    }

    /// Toast with a button `label` that sends `msg()`.
    fn action_toast(&mut self, title: &str, label: &str, msg: impl Fn() -> Msg + 'static) {
        let id = self.next_action;
        self.next_action += 1;
        self.actions.insert(id, Box::new(msg));
        // Old buttons expire with their toasts: keep the last few.
        self.actions.retain(|&k, _| k + 16 > id);
        self.toasts.push((title.to_owned(), label.to_owned(), id));
    }

    fn undo_toast(&mut self, title: &str, undo: impl Fn() -> Msg + 'static) {
        self.action_toast(title, &gettext("Undo"), undo);
    }

    /// Asks QML for a file dialog; it answers with command `reply` and `{path|paths, extra}`.
    fn dialog(&mut self, kind: &str, options: serde_json::Value) {
        self.dialogs.push((kind.to_owned(), options.to_string()));
    }

    fn dispatch(&mut self, msg: Msg) {
        self.dirty = true;
        match msg {
            Msg::Open => {
                let folder = settings::folder(settings::PHOTO_FOLDER);
                self.dialog(
                    "openMany",
                    serde_json::json!({
                        "title": gettext("Open Photo"),
                        "filters": [photo_filter()],
                        "folder": folder,
                        "reply": "rollAdd",
                    }),
                );
            }
            Msg::RollAddPaths(paths, open_at) => self.add_to_roll(paths, open_at),
            Msg::MergeHdr => self.dialog(
                "openMany",
                serde_json::json!({
                    "title": gettext("Merge Brackets to HDR"),
                    "filters": [photo_filter()],
                    "folder": settings::folder(settings::PHOTO_FOLDER),
                    "reply": "mergeHdrRun",
                }),
            ),
            Msg::MergeHdrRun(mut paths) => {
                if paths.len() < 2 {
                    return self.toast(&gettext("Pick two or more RAWs of the same scene at different exposures"));
                }
                paths.sort();
                let first = &paths[0];
                let stem = first.file_stem().unwrap_or_default().to_string_lossy();
                let out = free_path(first.parent().unwrap_or(std::path::Path::new(".")), &stem, "dng", "-HDR");
                self.toast(&gettext!("Merging {} photos to HDR…", paths.len()));
                let s = self.tx.clone();
                std::thread::spawn(move || {
                    let refs: Vec<&std::path::Path> = paths.iter().map(PathBuf::as_path).collect();
                    s.send(Msg::MergeHdrDone(rawvibes_core::merge::merge_files(&refs, &out).map(|_| out)));
                });
            }
            Msg::MergeHdrDone(Ok(path)) => {
                self.toast(&gettext!("HDR merge saved: {}", path.file_name().unwrap_or_default().to_string_lossy()));
                self.add_to_roll(vec![path], None);
            }
            Msg::MergeHdrDone(Err(e)) => self.toast(&gettext!("HDR merge failed: {}", e)),
            Msg::RollLoaded(path, Ok((small, profile, photo))) => {
                if let Some(it) = self.roll.iter_mut().find(|it| it.path == path) {
                    (it.small, it.encoded) = (Some(small), Some((profile, photo)));
                    self.upload_roll(false);
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
                if let Some(i) = self.roll.iter().position(|it| it.path == path) {
                    self.drop_from_roll(i);
                }
                self.toast(&gettext!("Cannot open {}: {}", path.display(), e));
            }
            Msg::RollSelect(i) => {
                if Some(i) != self.active {
                    self.load_active(i);
                }
            }
            Msg::RollRemove(i) => {
                if i >= self.roll.len() {
                    return;
                }
                let was_active = self.drop_from_roll(i);
                if was_active && self.roll.is_empty() {
                    self.load_gen += 1;
                    self.cancel_match();
                    (self.decoded, self.photo, self.path) = (None, None, None);
                    (self.history, self.undo_at) = (Vec::new(), 0);
                    (self.meta, self.source_meta) = (Meta::default(), None);
                    self.show_source();
                } else if was_active {
                    self.load_active(i.min(self.roll.len() - 1));
                }
            }
            Msg::RollFilter(f) => self.ui.roll_filter = f,
            Msg::Grid(on) => self.ui.grid = on && self.roll.len() > 1,
            Msg::Develop(edit) => self.edit_develop(edit),
            Msg::SaveSidecar(tick) if tick == self.save_tick => {
                self.record_edits();
                self.save_now();
            }
            Msg::SaveSidecar(_) => {}
            Msg::WriteSidecar(tick) if tick == self.save_tick => self.save_now(),
            Msg::WriteSidecar(_) => {}
            Msg::Close => {
                self.save_now();
                if let Some(cancel) = &self.exporting {
                    cancel.store(true, Ordering::Relaxed);
                }
                // The GPU worker ends with its channel; NVIDIA's exit handlers segfault if it is still tearing down.
                self.jobs = mpsc::channel().0;
                if let Some(t) = self.gpu_thread.take() {
                    let _ = t.join();
                }
            }
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
                self.layers_changed(Layers::Look, true);
                self.looks_loading = false;
                self.saved = Some(self.sidecar_state());
                self.reset_history();
                self.batch_ready();
            }
            Msg::AsInCamera(part) => self.as_in_camera(part),
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
                let extra = serde_json::json!({ "photo": photo, "name": l.name, "opacity": l.opacity });
                self.dialog(
                    "open",
                    serde_json::json!({
                        "title": gettext!("Find the LUT “{}”", l.name),
                        "filters": [lut_filter(&gettext("LUTs"))],
                        "folder": settings::folder(settings::LUT_FOLDER),
                        "reply": "cameraLutFound",
                        "extra": extra,
                    }),
                );
            }
            Msg::CameraLutFound(photo, l, p) => self.camera_lut_found(photo, l, p),
            Msg::CameraLutAdd(photo, ..) if Some(&photo) != self.path.as_ref() => {}
            Msg::CameraLutAdd(photo, l, p) => {
                if self.camera_curve.take().is_some_and(|(f, c)| f == photo && c == self.grade.curves[MASTER]) {
                    let mut grade = self.grade.clone();
                    grade.curves[MASTER] = neutral_curve(MASTER);
                    self.apply_grade(grade);
                }
                // Real Time LUTs take V-Log: the camera applies them to its log signal.
                let known = Some((Profile::VLog, l.opacity as f32 / 100.0));
                self.load_layer(Layers::Look, p, known);
            }
            Msg::CameraJpeg(path, _) if Some(&path) != self.path.as_ref() => {}
            Msg::CameraJpeg(path, Ok(reference)) => {
                let (Some(d), Some(as_shot)) = (self.decoded.clone(), self.as_shot()) else { return };
                self.camera_jpeg = Some((path.clone(), reference.clone()));
                let source = as_shot_render(&d, &as_shot, self.profile);
                self.show_match_thumb(&reference);
                self.ui.match_name = gettext("Camera JPEG");
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
                if let Some(at) = self.image_point(p).filter(|_| self.ui.picker) {
                    let detail = Box::new(self.applied_develop().detail);
                    self.send(Job::SampleRaw { at, radius: 4, detail });
                }
            }
            Msg::DevelopSettled(tick) if tick == self.develop_tick && self.drafting && self.refine => {
                self.drafting = false;
                self.encode_photo();
                self.request_view();
            }
            Msg::DevelopSettled(_) => {}
            Msg::AiReady => {
                self.encode_photo();
                self.request_view();
            }
            Msg::Refine(on) => {
                self.refine = on;
                if let Err(e) = settings::set_value(settings::REFINE_IDLE, if on { "1" } else { "0" }) {
                    self.toast(&gettext!("Cannot save the setting: {}", e));
                }
                if on && std::mem::take(&mut self.drafting) {
                    self.encode_photo();
                    self.request_view();
                }
            }
            Msg::Quality(i) => {
                self.quality = PreviewQuality::ALL[i as usize % PreviewQuality::ALL.len()];
                if let Err(e) = settings::set_value(settings::PREVIEW_QUALITY, self.quality.key()) {
                    self.toast(&gettext!("Cannot save the setting: {}", e));
                }
                self.encode_photo();
            }
            // Superseded: not loaded on the GPU, but the strip still needs its copy.
            Msg::RawRead(generation, path, _) if generation != self.load_gen => {
                let (s, profile) = (self.tx.clone(), self.profile);
                std::thread::spawn(move || {
                    let small = Decoded::load_max(&path, Some(ROLL_PX)).map(|d| roll_copy(&d, profile));
                    s.send(Msg::RollLoaded(path, small));
                });
            }
            Msg::RawRead(generation, path, Ok(raw)) => self.send(Job::LoadRaw(generation, path, raw)),
            Msg::RawRead(generation, path, Err(e)) => {
                self.input(Msg::RollLoaded(path.clone(), Err(e.clone())));
                self.input(Msg::Loaded(generation, path, Err(e)));
            }
            Msg::Loaded(generation, ..) if generation != self.load_gen => {}
            Msg::Loaded(_, path, Ok(decoded)) => self.loaded(path, decoded),
            Msg::Loaded(_, path, Err(e)) => {
                self.active = self.path.as_ref().and_then(|p| self.roll.iter().position(|it| it.path == *p));
                self.show_source();
                if let Some(b) = self.batch.as_mut().filter(|b| b.waiting.as_ref() == Some(&path)) {
                    b.waiting = None;
                    b.failed.push(format!("{}: {e}", file_label(&path)));
                    self.batch_next();
                }
            }
            Msg::Encoded(profile, photo) => {
                self.encoding = false;
                if std::mem::take(&mut self.encode_again) {
                    self.encode_photo();
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
            Msg::Band(b) => self.band = b.min(7),
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
                if ch < self.grade.curves.len() && c.len() >= 2 {
                    self.grade.curves[ch] = c;
                    self.render();
                }
            }
            Msg::ResetAll => {
                let before = (self.sidecar_state(), self.looks.clone());
                let develop = self.develop;
                (self.develop, self.lens_profile) = (Develop::default(), None);
                self.update_lens(false);
                if self.develop != develop {
                    self.develop_changed();
                }
                self.apply_grade(Grade::default());
                self.look_blend = Blend::Chain;
                self.reset_crop();
                self.crop_changed();
                self.replace_looks(Vec::new());
                self.set_offs(&[]);
                self.undo_toast(&gettext("All edits cleared"), move || Msg::RestoreEdits(Box::new(before.clone())));
            }
            Msg::RestoreEdits(edits) => self.restore_edits(*edits),
            Msg::MetaEdit(f, text) => {
                if self.path.is_some() && apply_meta(&mut self.meta, f, &text) {
                    self.schedule_save();
                }
            }
            Msg::LensDb(db) => {
                self.lens_db = db;
                self.update_lens(true);
            }
            Msg::LensSearch(text) => {
                self.ui.lens_results = self.lens_db.as_deref().map_or_else(Vec::new, |db| {
                    db.search(&text).into_iter().map(|l| (l.maker.clone(), l.model.clone())).collect()
                });
            }
            Msg::LensPick(pick) => {
                let LensView { focal, aperture, .. } = self.find_lens();
                self.lens_profile = pick.map(|(maker, model)| LensChoice { maker, model, focal, aperture });
                self.update_lens(true);
                self.schedule_save();
            }
            Msg::LensManual(focal, aperture) => {
                if self.lens_profile.is_none()
                    && let Some((maker, model)) = self.find_lens().lens
                {
                    self.lens_profile = Some(LensChoice { maker, model, ..Default::default() });
                }
                if let Some(c) = self.lens_profile.as_mut() {
                    (c.focal, c.aperture) = (focal, aperture);
                }
                self.update_lens(true);
                self.schedule_save();
            }
            Msg::SourceMeta(generation, ..) if generation != self.load_gen => {}
            Msg::SourceMeta(_, path, m) => {
                self.source_meta = m.map(|m| (path, m));
                self.update_lens(true);
            }
            Msg::Undo => self.step_history(true),
            Msg::Redo => self.step_history(false),
            Msg::Rate(..) | Msg::Mark(_) if self.path.is_none() => {}
            Msg::Rate(r, toggle) => {
                self.rating = if toggle && r == self.rating { 0 } else { r.min(5) };
                self.show_marks();
                self.schedule_save();
            }
            Msg::Mark(l) => {
                self.label = l;
                self.show_marks();
                self.schedule_save();
            }
            Msg::CopyEdits if self.path.is_some() => {
                self.clipboard = Some((self.sidecar_state(), self.looks.clone()));
                self.toast(&gettext("Edits copied"));
            }
            Msg::CopyEdits => {}
            Msg::PasteEdits => {
                let Some((sc, looks)) = self.clipboard.clone().filter(|_| self.path.is_some()) else { return };
                self.record_edits();
                let sc = pasted(&sc, &self.sidecar_state());
                self.restore_edits((sc, looks));
                self.toast(&gettext("Edits pasted: Ctrl+Z undoes it"));
            }
            Msg::PasteEditsAll => {
                let Some(clip) = self.clipboard.clone() else { return };
                if self.path.is_some() && self.shows(self.rating, self.label) {
                    self.input(Msg::PasteEdits);
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
            Msg::BatchTo(dir) => {
                let left: Vec<PathBuf> = self
                    .roll
                    .iter()
                    .filter(|it| self.shows(it.rating, it.label))
                    .map(|it| it.path.clone())
                    .rev()
                    .collect();
                let total = left.len();
                let back = self.path.clone();
                self.batch = Some(Batch { left, dir, total, failed: Vec::new(), back, waiting: None, stop: false });
                self.ui.export.busy = true;
                self.batch_next();
            }
            Msg::RestoreGrade(grade) => self.apply_grade(*grade),
            Msg::MixerMono(on) => {
                self.grade.params.mixer_mono = if on { 1.0 } else { 0.0 };
                self.render();
            }
            Msg::MixerFree(free) => {
                self.grade.params.mixer_free = if free { 1.0 } else { 0.0 };
                self.render();
            }
            Msg::Mixer(i, delta) => {
                *mixer_value_mut(&mut self.grade.params, i.min(11)) = delta as f32;
                self.render();
            }
            Msg::MatchChoose => {
                if !self.has_source() {
                    return self.toast(&no_photo_to_match());
                }
                self.dialog(
                    "open",
                    serde_json::json!({
                        "title": gettext("Choose Reference"),
                        "filters": [photo_filter()],
                        "folder": settings::folder(settings::PHOTO_FOLDER),
                        "reply": "matchReference",
                    }),
                );
            }
            Msg::MatchReference(path) => {
                self.ui.matching = "busy";
                let s = self.tx.clone();
                std::thread::spawn(move || {
                    // References are display images (a still, a JPEG); RAWs are rendered neutrally.
                    let r = Decoded::load_max(&path, Some(1024)).map(|d| d.encode(Profile::Standard, &Develop::default()));
                    s.send(Msg::MatchLoaded(path, r));
                });
            }
            Msg::MatchLoaded(path, Ok(reference)) => {
                self.show_match_thumb(&reference);
                self.ui.match_name = file_label(&path);
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
                self.cancel_match();
                self.render();
            }
            Msg::MatchAmount(v) => {
                self.grade.params.match_amount = v as f32;
                self.render();
            }
            Msg::Overlay(on) => {
                self.overlay = on;
                self.render();
            }
            Msg::SkinLine(on) => self.ui.skin = on,
            Msg::PresetsRefresh => {
                self.ui.presets = self.presets().into_iter().map(|(p, path)| (p.name, path)).collect();
            }
            Msg::PresetSave => {
                let suggestion = self.path.as_deref().and_then(stem_label).unwrap_or_default();
                self.dialog("presetName", serde_json::json!({ "name": suggestion, "reply": "presetSaveAs" }));
            }
            Msg::PresetSaveAs(name) => {
                let name = name.trim().to_owned();
                if !name.is_empty() {
                    self.input(Msg::PresetWrite(Box::new(self.current_preset(name)), false, false))
                }
            }
            Msg::PresetWrite(preset, force, apply) => {
                let path = settings::presets_dir().join(preset::file_name(&preset.name));
                let existing = self.presets();
                if !force && existing.iter().any(|(p, at)| *at == path || p.name == preset.name) {
                    let name = preset.name.clone();
                    let replace = move || Msg::PresetWrite(preset.clone(), true, apply);
                    let title = gettext!("A preset named “{}” exists", name);
                    return self.action_toast(&title, &gettext("Replace"), replace);
                }
                for (p, at) in existing {
                    if p.name == preset.name && at != path {
                        let _ = std::fs::remove_file(at);
                    }
                }
                match preset.save(&path) {
                    Ok(()) if apply => self.input(Msg::PresetApply(path)),
                    Ok(()) => self.toast(&gettext!("Preset “{}” saved", preset.name)),
                    Err(e) => self.toast(&gettext!("Cannot save the preset: {}", e)),
                }
                self.input(Msg::PresetsRefresh);
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
                let Ok(text) = std::fs::read_to_string(&path) else { return };
                if let Err(e) = std::fs::remove_file(&path) {
                    return self.toast(&gettext!("Cannot delete the preset: {}", e));
                }
                self.input(Msg::PresetsRefresh);
                let name = Preset::from_json(&text).map(|p| p.name).unwrap_or_default();
                self.undo_toast(&gettext!("Preset “{}” deleted", name), move || {
                    Msg::PresetRestore(path.clone(), text.clone())
                });
            }
            Msg::PresetRestore(path, text) => {
                if let Err(e) = std::fs::write(&path, text) {
                    self.toast(&gettext!("Cannot restore the preset: {}", e));
                }
                self.input(Msg::PresetsRefresh);
            }
            Msg::PresetImport => self.dialog(
                "open",
                serde_json::json!({
                    "title": gettext("Import Preset"),
                    "filters": [preset_filter()],
                    "reply": "presetImported",
                }),
            ),
            Msg::PresetImported(p) => match Preset::load(&p) {
                Ok(preset) => self.input(Msg::PresetWrite(Box::new(preset), false, true)),
                Err(e) => self.toast(&gettext!("Not a valid preset: {}", e)),
            },
            Msg::PresetExport => {
                let stem = self.path.as_deref().and_then(stem_label).unwrap_or_else(|| "preset".into());
                self.dialog(
                    "save",
                    serde_json::json!({
                        "title": gettext("Export Preset"),
                        "filters": [preset_filter()],
                        "name": preset::file_name(&stem),
                        "reply": "presetExportTo",
                    }),
                );
            }
            Msg::PresetExportTo(path) => match self.current_preset(stem_label(&path).unwrap_or_default()).save(&path) {
                Ok(()) => self.toast(&gettext!("Preset saved: {}", path.display())),
                Err(e) => self.toast(&gettext!("Cannot save the preset: {}", e)),
            },
            Msg::ToastAction(id) => {
                if let Some(msg) = self.actions.get(&id).map(|f| f()) {
                    self.input(msg);
                }
            }
            Msg::Compare(on) => {
                self.compare = on;
                self.refresh();
            }
            Msg::Split(x) => {
                if self.compare {
                    self.split = x.clamp(0.0, 1.0);
                    self.refresh();
                }
            }
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
                let what = match what {
                    What::Grade => "grade",
                    What::Identity => "identity",
                    What::Lab => "lab",
                };
                self.dialog(
                    "save",
                    serde_json::json!({
                        "title": gettext!("Export {}", label),
                        "filters": [format!("{label} (*.{})", format.ext())],
                        "name": format!("{stem}.{}", format.ext()),
                        "folder": settings::folder(settings::LUT_FOLDER),
                        "reply": "exportLutTo",
                        "extra": { "format": format.ext(), "size": size, "what": what },
                    }),
                );
            }
            Msg::ExportTo(format, size, what, path) => {
                let title = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                match what {
                    What::Identity => {
                        let s = self.tx.clone();
                        std::thread::spawn(move || {
                            let r = Lut3d::identity(size, format).save(&path).map(|_| path);
                            s.send(Msg::Gpu(Done::Exported(r)));
                        });
                    }
                    What::Grade => {
                        let job = Job::Export {
                            grade: self.graded(),
                            look: self.look_stack(self.look_layers()),
                            size,
                            format,
                            title,
                            path,
                        };
                        self.send(job)
                    }
                    What::Lab => {
                        let stack = self.lab_stack(self.lab_input);
                        self.send(Job::ExportLab { stack, size, format, title, path })
                    }
                }
            }
            Msg::ExportPhoto(_) if self.exporting.is_some() => self.ui.export.open = true,
            Msg::ExportPhoto(format) => {
                let Some(crop) = self.crop_region().filter(|_| !self.show_chart && self.path.is_some()) else {
                    return self.toast(&gettext("Open a photo to export it"));
                };
                let e = &mut self.ui.export;
                if let Some(f) = format {
                    e.format = Some(f);
                }
                e.roll = self.roll.iter().filter(|it| self.ui.roll_filter == 0 || shown(self.ui.roll_filter, it.rating, it.label)).count();
                e.size = Decoded::fit_size(&crop, u32::MAX);
                e.open = true;
                self.export_frame_request();
            }
            Msg::ExportSettings(format, quality, all) => {
                let hdr_changed = self.ui.export.format.is_none_or(|f| f.hdr() != format.hdr());
                let e = &mut self.ui.export;
                (e.format, e.quality, e.all) = (Some(format), quality.clamp(1, 100), all);
                match &self.export_frame {
                    Some(f) if f.hdr() == format.hdr() && !hdr_changed => self.export_estimate(),
                    _ => self.export_frame_request(),
                }
            }
            Msg::HdrPreview(on) => {
                self.hdr_preview = on;
                let _ = settings::set_value(settings::HDR_PREVIEW, if on { "1" } else { "0" });
                self.render();
            }
            Msg::Headroom(stops) => {
                self.headroom = stops.clamp(1.0, 3.0);
                self.render();
                self.timers.after(Duration::from_millis(500), Msg::HeadroomSave(self.headroom));
            }
            Msg::HeadroomSave(stops) if stops == self.headroom => {
                let _ = settings::set_value(settings::HDR_HEADROOM, &format!("{stops:.2}"));
            }
            Msg::HeadroomSave(_) => {}
            Msg::ExportEstimateRun(generation, w, h) if generation == self.export_gen => {
                let e = &self.ui.export;
                let format = e.format.unwrap_or(PhotoFormat::Jpeg);
                let Some(frame) = self.export_frame.clone().filter(|f| f.hdr() == format.hdr()) else { return };
                let (quality, s) = (e.quality, self.tx.clone());
                std::thread::spawn(move || {
                    let scale = (w as f64 * h as f64) / (frame.w as f64 * frame.h as f64);
                    let n = export::encoded_size(&frame, format, quality) as f64 * scale;
                    s.send(Msg::ExportEstimate(generation, n as u64));
                });
            }
            Msg::ExportEstimateRun(..) => {}
            Msg::ExportEstimate(generation, n) if generation == self.export_gen => {
                self.ui.export.estimate = Some(gettext!("about {}", format_size(n)))
            }
            Msg::ExportEstimate(..) => {}
            Msg::ExportGo => self.export_go(),
            Msg::ExportPhotoTo(path) => {
                self.export_to(path);
            }
            Msg::ExportClosed => {
                self.ui.export.open = false;
                if let Some(cancel) = &self.exporting {
                    cancel.store(true, Ordering::Relaxed);
                }
                if let Some(b) = self.batch.as_mut() {
                    b.stop = true;
                    if self.exporting.is_none() {
                        self.batch_stop();
                    }
                }
            }
            Msg::ChooseFolder => self.dialog(
                "folder",
                serde_json::json!({
                    "title": gettext("LUT folder"),
                    "folder": settings::folder(settings::LUT_FOLDER),
                    "reply": "lutFolder",
                }),
            ),
            Msg::Folder(dir) => {
                if let Err(e) = settings::set_folder(settings::LUT_FOLDER, &dir) {
                    self.toast(&gettext!("Cannot save the LUT folder: {}", e));
                }
                // Watched before the scan, so files added while it runs trigger another one.
                self.watch_folder(&dir);
                let s = self.tx.clone();
                std::thread::spawn(move || {
                    let (luts, errors, truncated) = Lut3d::load_dir(&dir);
                    s.send(Msg::GalleryLoaded(luts, errors, truncated));
                });
            }
            Msg::GalleryLoaded(luts, errors, truncated) => self.gallery_loaded(luts, errors, truncated),
            Msg::Group(sel) => {
                self.group = (sel as usize).checked_sub(1).and_then(|i| self.group_names.get(i)).cloned();
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
                    Off::Looks => self.layers_changed(Layers::Look, false),
                    Off::Develop(_) if self.camera().is_some() => self.develop_changed(),
                    Off::Develop(_) => {}
                    Off::Crop => self.crop_changed(),
                }
                self.schedule_save();
            }
            Msg::Browse(on) => {
                self.browsing = on;
                if !on {
                    self.drop_candidate();
                }
                self.refresh_gallery();
            }
            Msg::Reload => match settings::folder(settings::LUT_FOLDER) {
                Some(dir) => self.input(Msg::Folder(dir)),
                None => self.input(Msg::ChooseFolder),
            },
            Msg::Search(text) => {
                self.query = text.to_lowercase();
                self.hide_filtered_selection();
            }
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
                let (p, s) = (entry.path.clone(), self.tx.clone());
                std::thread::spawn(move || s.send(Msg::PreviewLoaded(i, Lut3d::load(&p).map(Arc::new))));
            }
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
                self.timers.after(HOVER_REST, Msg::HoverSettled(self.hover_tick, over));
            }
            Msg::HoverSettled(tick, over) if tick == self.hover_tick => self.input(Msg::Preview(over)),
            Msg::HoverSettled(..) => {}
            Msg::Commit(i) => {
                let path = self.luts.get(i).map(|l| &l.path);
                // A double-click activates twice: the second adds nothing.
                if self.flash_new_look
                    || path.is_some_and(|p| self.looks.last().and_then(|l| l.path.as_ref()) == Some(p))
                {
                    return;
                }
                self.flash_new_look = true;
                match self.candidate.take().filter(|c| c.0 == i) {
                    Some((_, lut)) => {
                        let path = self.luts.get(i).map(|l| l.path.clone());
                        self.push_lut(Layers::Look, lut, path, None)
                    }
                    None => {
                        if let Some(p) = self.luts.get(i).map(|e| e.path.clone()) {
                            self.load_layer(Layers::Look, p, None);
                        }
                    }
                }
                self.drop_candidate();
            }
            Msg::Escape => {
                if self.ui.picker {
                    self.ui.picker = false;
                } else if self.editing.is_some() {
                    self.input(Msg::CropCancel);
                } else if self.candidate.is_some() || self.preview_req.is_some() {
                    self.clear_selection();
                } else if self.browsing {
                    self.input(Msg::Browse(false));
                }
            }
            Msg::Profile(profile) if profile == self.profile && self.photo.as_ref().is_some_and(|p| p.1 == profile) => {}
            Msg::Profile(profile) => {
                self.profile = profile;
                self.schedule_save();
                self.reencode_roll();
                if self.show_chart {
                    self.input(Msg::Chart(true));
                }
                self.encode_photo();
            }
            Msg::Chart(on) => {
                self.show_chart = on;
                if !on || self.chart.as_ref().is_some_and(|c| c.1 == self.profile) {
                    self.show_source();
                } else {
                    let (s, profile) = (self.tx.clone(), self.profile);
                    std::thread::spawn(move || s.send(Msg::ChartReady(profile, Arc::new(test_chart(profile)))));
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
                LabAdd::Selected => match self.selected.and_then(|i| self.luts.get(i)).map(|e| e.path.clone()) {
                    Some(p) => self.load_layer(which, p, None),
                    None => self.toast(&gettext("Select a LUT in the browser first")),
                },
                LabAdd::File => self.dialog(
                    "open",
                    serde_json::json!({
                        "title": gettext("Add LUT"),
                        "filters": [lut_filter(&gettext("LUTs (.cube, .vlt, HALD .png)"))],
                        "folder": settings::folder(settings::LUT_FOLDER),
                        "reply": "layerFile",
                        "extra": { "which": if which == Layers::Lab { "lab" } else { "look" } },
                    }),
                ),
                LabAdd::Grade => self.push_layer(which, LabKind::Grade, Profile::Standard),
                LabAdd::Convert => self.push_layer(which, LabKind::Convert(Profile::Standard), Profile::Standard),
            },
            Msg::OpenInLab => {
                if self.selected.is_some() {
                    self.ui.tab = "lab".into();
                    self.input(Msg::Page(Page::Lab));
                }
                self.input(Msg::LayerAdd(Layers::Lab, LabAdd::Selected));
            }
            Msg::LayerLoaded(which, p, known, Ok(lut)) => self.push_lut(which, Arc::new(lut), Some(p), known),
            Msg::LayerLoaded(_, p, _, Err(e)) => {
                self.flash_new_look = false;
                self.toast(&gettext!("Cannot read {}: {}", p.display(), e))
            }
            Msg::LayerRemove(which, i) => {
                if i < self.layers_mut(which).len() {
                    self.layers_mut(which).remove(i);
                    self.layers_changed(which, true);
                }
            }
            Msg::LayerMove(which, i, up) => {
                let j = if up { i.checked_sub(1) } else { Some(i + 1) };
                if let Some(j) = j.filter(|&j| j < self.layers_mut(which).len()) {
                    self.layers_mut(which).swap(i, j);
                    self.layers_changed(which, true);
                }
            }
            Msg::LayerIntensity(which, i, v) => {
                if let Some(l) = self.layers_mut(which).get_mut(i) {
                    l.intensity = (v / 100.0).clamp(0.0, 1.0) as f32;
                    self.layers_changed(which, false);
                }
            }
            Msg::LayerProfile(which, i, p) => {
                if let Some(l) = self.layers_mut(which).get_mut(i) {
                    match &mut l.kind {
                        LabKind::Convert(to) => *to = p,
                        _ => l.input = p,
                    }
                    self.layers_changed(which, false);
                }
            }
            Msg::LabInput(p) => self.lab_input = p,
            Msg::LookBlend(b) => {
                self.look_blend = b;
                self.show_looks();
                self.refresh_gallery();
            }
            Msg::LayerOn(which, i, on) => {
                if let Some(l) = self.layers_mut(which).get_mut(i) {
                    l.on = on;
                    self.layers_changed(which, false);
                }
            }
            Msg::FolderChanged => {
                self.folder_changes += 1;
                self.timers.after(Duration::from_millis(500), Msg::FolderSettled(self.folder_changes));
            }
            Msg::FolderSettled(n) if n == self.folder_changes => match settings::folder(settings::LUT_FOLDER) {
                Some(dir) => self.input(Msg::Folder(dir)),
                None => self.toast(&gettext("The LUT folder was moved or deleted: press F5 to choose it again")),
            },
            Msg::FolderSettled(_) => {}
            Msg::LabBlend(b) => {
                self.lab_blend = b;
                self.render_lab();
            }
            Msg::Tab(tab) => {
                let lab = tab == "lab";
                self.ui.tab = tab;
                let page = if lab { Page::Lab } else { Page::Grade };
                if page != self.page {
                    self.input(Msg::Page(page));
                }
            }
            Msg::Page(page) => {
                self.page = page;
                if !matches!(page, Page::Grade) {
                    self.set_zoom(None);
                }
                self.refresh();
            }
            Msg::Gpu(done) => self.on_gpu(done),
            Msg::Zoom(cmd) => self.on_zoom(cmd),
            Msg::CanvasSize(area) => {
                if area != self.canvas && area[0] >= 1.0 && area[1] >= 1.0 {
                    self.canvas = area;
                    self.request_view();
                }
            }
            Msg::CropEdit(on) if on == self.editing.is_some() => {}
            Msg::CropEdit(true) => {
                if self.decoded.is_none() || self.show_chart {
                    return;
                }
                self.set_zoom(None);
                self.editing = Some((self.crop, self.ratio, self.ratio_row));
                self.crop_changed();
            }
            Msg::CropEdit(false) => {
                self.set_zoom(None);
                self.editing = None;
                self.crop_changed();
            }
            Msg::CropCancel => {
                if let Some((crop, ratio, row)) = self.editing {
                    self.restore_crop(crop, ratio, row);
                    self.input(Msg::CropEdit(false));
                }
            }
            Msg::CropRatio(i) => {
                let ratio = PRESETS.get(i).map_or(Ratio::Free, |p| p.1);
                self.ratio_row = i as u32;
                if ratio != self.ratio {
                    self.ratio = ratio;
                    self.fit_crop();
                    self.input(Msg::CropEdit(true));
                    self.crop_changed();
                }
            }
            Msg::CropSwap => {
                if let Ratio::Fixed(a, b) = self.ratio {
                    self.ratio = Ratio::Fixed(b, a);
                    self.fit_crop();
                    self.crop_changed();
                }
            }
            Msg::CropAngle(a) if a == self.crop.angle => {}
            Msg::CropAngle(a) => {
                if let Some((w, h)) = self.size() {
                    self.crop = crop::straighten(self.crop, a.clamp(-45.0, 45.0), w, h);
                }
                self.crop_changed();
            }
            Msg::CropRect(rect) => {
                self.crop.rect = rect;
                if let Some((w, h)) = self.size() {
                    self.crop = crop::clamp(self.crop, w, h);
                }
                self.crop_changed();
            }
            Msg::CropReset => {
                self.restore_crop(Crop::default(), Ratio::Free, 0);
                self.crop_changed();
            }
            Msg::ViewReady(generation, profile, photo) => {
                let shown = self.photo.as_ref().map(|p| p.1);
                if generation == self.view_gen && self.view_zoom().is_some() && shown == Some(profile) {
                    self.send(Job::SetView(photo));
                    self.sent = None;
                    self.render();
                }
            }
            Msg::Background(name) => {
                let _ = settings::set_value(settings::BACKGROUND, &name);
            }
            Msg::MetaPolicy(tag) => {
                let _ = settings::set_value(settings::METADATA_POLICY, &tag);
            }
            Msg::ExportSuffix(s) => {
                let _ = settings::set_value(settings::EXPORT_SUFFIX, &s);
            }
            Msg::AuthorDefault(key, value) => {
                let key = match key.as_str() {
                    "author" => settings::AUTHOR,
                    "copyright" => settings::COPYRIGHT,
                    "email" => settings::AUTHOR_EMAIL,
                    "website" => settings::AUTHOR_WEBSITE,
                    _ => return,
                };
                let _ = settings::set_value(key, value.trim());
            }
        }
    }
}

/// The filmstrip copy of a decoded photo: small, and encoded for `profile`.
fn roll_copy(d: &Decoded, profile: Profile) -> (Decoded, Profile, Arc<Photo>) {
    let small = d.downscaled(ROLL_PX);
    let photo = small.encode(profile, &Develop::default());
    (small, profile, photo)
}

pub fn file_label(path: &std::path::Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn stem_label(path: &std::path::Path) -> Option<String> {
    path.file_stem().map(|s| s.to_string_lossy().into_owned())
}

/// `<stem><suffix>.<ext>` in `dir`, or `<stem>-2<suffix>.<ext>` and on: never an existing file.
fn free_path(dir: &std::path::Path, stem: &str, ext: &str, suffix: &str) -> PathBuf {
    (1..)
        .map(|n| dir.join(edit_name(&if n == 1 { stem.to_string() } else { format!("{stem}-{n}") }, ext, suffix)))
        .find(|p| !p.exists())
        .expect("unbounded")
}

/// Name of an exported photo: never the original's own.
fn edit_name(stem: &str, ext: &str, suffix: &str) -> String {
    // A suffix is part of a file name: no folders, never empty (it would overwrite the photo's own JPEG).
    let suffix: String = suffix.trim().chars().filter(|c| !matches!(c, '/' | '\\')).collect();
    let suffix = if suffix.is_empty() { "_edit" } else { &suffix };
    format!("{stem}{suffix}.{ext}")
}

/// The export file name suffix from Preferences.
pub fn export_suffix() -> String {
    settings::value(settings::EXPORT_SUFFIX).unwrap_or_default()
}

pub fn export_policy() -> rawvibes_core::meta::Policy {
    settings::value(settings::METADATA_POLICY)
        .and_then(|t| rawvibes_core::meta::Policy::from_tag(&t))
        .unwrap_or_default()
}

/// The HDR headroom in stops (1–3), as last set.
pub fn headroom_setting() -> f32 {
    settings::value(settings::HDR_HEADROOM)
        .and_then(|q| q.parse().ok())
        .unwrap_or(export::HDR_HEADROOM)
        .clamp(1.0, 3.0)
}

/// The last photo export format and JPEG quality used.
pub fn remembered_export() -> (PhotoFormat, u8) {
    let format = settings::value(settings::EXPORT_FORMAT).and_then(|t| PhotoFormat::from_tag(&t));
    let quality = settings::value(settings::JPEG_QUALITY).and_then(|q| q.parse().ok());
    (format.unwrap_or(PhotoFormat::Jpeg), quality.unwrap_or(export::JPEG_QUALITY).clamp(1, 100))
}

pub const COPYRIGHT_TEMPLATE: &str = "© {year} {creator}";

/// The author defaults of Preferences, as metadata.
pub fn author_defaults() -> Meta {
    let v = |k| settings::value(k).unwrap_or_default();
    Meta {
        creator: v(settings::AUTHOR),
        copyright: settings::value(settings::COPYRIGHT).unwrap_or_else(|| COPYRIGHT_TEMPLATE.into()),
        email: v(settings::AUTHOR_EMAIL),
        website: v(settings::AUTHOR_WEBSITE),
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

/// A filmstrip card's grade: its sidecar's, with switched-off sections neutral.
fn card_grade(sc: &Sidecar) -> Grade {
    let mut g = sc.grade.clone();
    bypass_off(&mut g, &sc.off);
    g
}

fn bypass_off(g: &mut Grade, off: &[Off]) {
    for o in off {
        if let Off::Grade(s) = o {
            g.bypass(*s);
        }
    }
}

/// The photo as the camera shot it, for matching the camera JPEG.
fn as_shot_render(d: &Decoded, as_shot: &Develop, profile: Profile) -> Arc<Photo> {
    let (w, h) = d.size();
    d.encode_fit(&Region::whole(w, h), 1024, profile, as_shot)
}

/// Settings key remembering where the camera LUT `name` was found.
fn camera_lut_key(name: &str) -> String {
    let safe: String =
        name.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    format!("camera-lut-{safe}")
}

pub fn mixer_value_mut(p: &mut Params, i: usize) -> &mut f32 {
    if i < 9 { &mut p.mixer[i] } else { &mut p.mono_mix[i - 9] }
}

pub fn wheel_mut(p: &mut Params, i: usize) -> &mut [f32; 4] {
    match i {
        0 => &mut p.lift,
        1 => &mut p.gamma,
        2 => &mut p.gain,
        _ => &mut p.offset,
    }
}

/// Puts `text` in `f` of `m`; false when it already held it.
fn apply_meta(m: &mut Meta, f: MetaField, text: &str) -> bool {
    let slot = match f {
        MetaField::Title => &mut m.title,
        MetaField::Caption => &mut m.caption,
        MetaField::Creator => &mut m.creator,
        MetaField::Copyright => &mut m.copyright,
        MetaField::City => &mut m.city,
        MetaField::Country => &mut m.country,
        MetaField::Keywords => {
            let k: Vec<String> = text.split(',').map(str::trim).filter(|k| !k.is_empty()).map(String::from).collect();
            if k == m.keywords {
                return false;
            }
            m.keywords = k;
            return true;
        }
    };
    if slot == text {
        return false;
    }
    *slot = text.to_string();
    true
}

/// Filter `f` of the filmstrip: 0 all, 1–5 at least that many stars, then the colour labels.
pub fn shown(f: u32, rating: u8, label: Label) -> bool {
    match f {
        0 => true,
        1..=5 => rating as u32 >= f,
        _ => Label::ALL.get(f as usize - 5) == Some(&label),
    }
}

fn no_photo_to_match() -> String {
    gettext("Open a photo first: the match is computed on it")
}

fn photo_filter() -> String {
    let exts: Vec<String> = rawvibes_core::photo::RASTER_EXTS
        .iter()
        .chain(rawvibes_core::photo::RAW_EXTS.iter())
        .flat_map(|e| [format!("*.{e}"), format!("*.{}", e.to_uppercase())])
        .collect();
    format!("{} ({})", gettext("Photos and RAWs"), exts.join(" "))
}

fn lut_filter(label: &str) -> String {
    format!("{label} (*.cube *.vlt *.png *.CUBE *.VLT *.PNG)")
}

fn preset_filter() -> String {
    format!("{} (*.{})", gettext("RawVibes presets"), preset::EXT)
}

/// Bytes as "4.2 MB" (decimal units, as GLib's format_size).
fn format_size(n: u64) -> String {
    match n {
        n if n < 1000 => format!("{n} bytes"),
        n if n < 1_000_000 => format!("{:.1} kB", n as f64 / 1e3),
        n if n < 1_000_000_000 => format!("{:.1} MB", n as f64 / 1e6),
        n => format!("{:.1} GB", n as f64 / 1e9),
    }
}

/// Installed: `<prefix>/share/rawvibes/lensfun.json`; dev: `make lensfun` puts it in `target/`.
fn lens_db_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.parent()?.join("share/rawvibes/lensfun.json")))
        .filter(|p| p.exists())
        .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../target/lensfun.json")))
}

#[allow(unused_imports)]
use {lut as _, Source as _, Layer as _, MAX_LIBRARY as _};
