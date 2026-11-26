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
use rawvibes_core::lab::Blend;
use rawvibes_core::lensdb;
use rawvibes_core::lut::{Format, Lut3d, LutEntry};
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
    Scroll {
        dy: f64,
        at: [f32; 2],
    },
    /// Drag by `d` with mouse `button` (1 primary, 2 middle).
    Pan {
        d: [f32; 2],
        button: u32,
    },
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
    SaveSidecar,
    WriteSidecar,
    Close,
    SidecarLooks(u64, Vec<(LookRef, Result<Arc<Lut3d>, String>)>),
    CanvasClick([f32; 2]),
    DevelopSettled,
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
    HoverSettled(Option<usize>),
    Commit(usize),
    Escape,
    Tab(String),
    LayerAdd(Layers, LabAdd),
    OpenInLab,
    LayerLoaded(Layers, PathBuf, Option<(Profile, f32)>, Result<Lut3d, String>),
    /// A LUT file chosen for a stack: read off the GUI thread.
    LayerFile(Layers, PathBuf),
    /// The newly added look has flashed long enough.
    FlashDone,
    LayerRemove(Layers, usize),
    LayerMove(Layers, usize, bool),
    LayerIntensity(Layers, usize, f64),
    LayerProfile(Layers, usize, Profile),
    LabInput(Profile),
    LabBlend(Blend),
    LookBlend(Blend),
    LayerOn(Layers, usize, bool),
    FolderChanged,
    FolderSettled,
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
    /// The HDR window's swapchain: HDR or not, SDR white on it (a multiplier on scRGB,
    /// cd/m² on HDR10), and whether it is HDR10.
    HdrWhite(bool, f32, bool),
    Headroom(f32),
    HeadroomSave,
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
    RollTick,
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
    /// Crop overlay: a press at a point of the frame (0–1) with a grab tolerance, a drag
    /// from it, and a level line drawn on the frame (its screen delta).
    CropPress([f32; 2], [f32; 2]),
    CropDrag([f32; 2]),
    CropLevel(f32, f32),
    /// Straighten along a line: the next drag on the frame draws it.
    Level(bool),
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
    /// A dialog's answer (cancelled dialogs send none).
    DialogAnswer(i32, DialogAnswer),
    /// Preferences.
    Background(String),
    MetaPolicy(String),
    ExportSuffix(String),
    AuthorDefault(String, String),
}

/// What the user chose in a dialog: files, or a typed name.
#[derive(Debug, Default)]
pub struct DialogAnswer {
    pub paths: Vec<PathBuf>,
    pub name: String,
    /// Closed without a choice: the request is forgotten.
    pub cancelled: bool,
}

impl DialogAnswer {
    pub fn path(self) -> Option<PathBuf> {
        self.paths.into_iter().next()
    }
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
    /// Stable across removals: its frames are stored under image://rv/roll/<id>.
    pub id: u64,
}

/// Preferences the view shows, read once and kept as they are changed: not read from disk per publish.
pub struct Prefs {
    pub as_in_camera: bool,
    pub lut_folder: Option<PathBuf>,
    pub policy: rawvibes_core::meta::Policy,
    pub suffix: String,
    pub author: Meta,
    pub background: String,
}

impl Prefs {
    fn load() -> Self {
        Prefs {
            as_in_camera: settings::flag(settings::AS_IN_CAMERA, false),
            lut_folder: settings::folder(settings::LUT_FOLDER),
            policy: export_policy(),
            suffix: export_suffix(),
            author: author_defaults(),
            background: settings::value(settings::BACKGROUND).unwrap_or_else(|| "default".into()),
        }
    }
}

/// What the GTK UI kept in its widgets: plain state QML draws from.
#[derive(Default)]
pub struct UiState {
    pub tab: String,
    /// "empty", "loading" or "image".
    pub canvas: &'static str,
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

/// What one round of messages leaves QML to hear.
pub struct Published {
    pub state: Option<String>,
    pub frames: Option<String>,
    pub toasts: Vec<(String, String, i32)>,
    pub dialogs: Vec<(String, String)>,
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
    flash_new_look: bool,
    base_key: Option<BaseKey>,
    gallery_stale: bool,
    pub lab_input: Profile,
    pub lab_blend: Blend,
    pub look_blend: Blend,
    /// Watches the LUT folder; its events are debounced into `Msg::FolderSettled`.
    watcher: Option<notify::RecommendedWatcher>,
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
    match_file: bool,
    match_gen: u64,
    pub crop: Crop,
    pub ratio: Ratio,
    /// While editing the crop: the crop, ratio and ratio row before editing (Escape restores them).
    pub editing: Option<(Crop, Ratio, u32)>,
    pub ratio_row: u32,
    /// A crop drag: the handle (`None`: drawing a new crop), the crop at its start, the press point.
    crop_grab: Option<(Option<crop::Handle>, Crop, [f32; 2])>,
    pub levelling: bool,
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
    camera_match: Option<(PathBuf, Match)>,
    camera_curve: Option<(PathBuf, Curve)>,
    camera_jpeg: Option<(PathBuf, Arc<Photo>)>,
    camera_job: Option<PathBuf>,
    saved: Option<Sidecar>,
    looks_loading: bool,
    /// "As in camera" was asked on open and its tone curve has not come back yet: a batch export waits for it.
    camera_pending: bool,
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
    pub prefs: Prefs,
    next_roll_id: u64,
    /// The state as QML last got it: an unchanged one is not sent again.
    published: String,
    /// Each frame's key in the image provider and its generation, as `image://rv/<key>/<generation>`.
    frames: std::collections::BTreeMap<String, u64>,
    frame_count: u64,
    frames_dirty: bool,
    /// The curves as last published, with their points and samples as JSON: sampled again only when they change.
    curve_cache: std::cell::RefCell<Option<(Vec<Curve>, serde_json::Value, serde_json::Value)>>,
    tx: Tx,
    timers: Timers,
    /// Messages the update itself queued (GTK's `sender.input`), run before publishing.
    queue: VecDeque<Msg>,
    toasts: Vec<(String, String, i32)>,
    dialogs: Vec<(String, String)>,
    actions: HashMap<i32, Box<dyn Fn() -> Msg>>,
    /// Dialogs waiting for an answer, by request id.
    answers: HashMap<i32, Box<dyn FnOnce(DialogAnswer) -> Option<Msg>>>,
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
        // The HDR rendition is converted for Qt's HDR surface here, on the GPU worker, not the GUI thread.
        let (jobs, gpu_thread) = gpu::spawn(move |mut d| {
            if let Done::Graded { hdr, .. } = &mut d {
                // Converted only for a surface that shows it; an SDR one gets the SDR frame.
                match hdr.as_mut().filter(|_| crate::hdr::surface()) {
                    Some(h) => crate::hdr::convert(h),
                    None => *hdr = None,
                }
            }
            g.send(Msg::Gpu(d))
        });
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
            flash_new_look: false,
            base_key: None,
            gallery_stale: true,
            lab_input: Profile::Standard,
            lab_blend: Blend::Chain,
            look_blend: Blend::Chain,
            watcher: None,
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
            hdr_preview: settings::flag(settings::HDR_PREVIEW, false),
            headroom: headroom_setting(),
            sent: None,
            load_gen: 0,
            gallery_gen: 0,
            overlay: false,
            roll: Vec::new(),
            active: None,
            roll_gen: 0,
            roll_sent: None,
            match_file: false,
            match_gen: 0,
            crop: Crop::default(),
            ratio: Ratio::Free,
            editing: None,
            ratio_row: 0,
            crop_grab: None,
            levelling: false,
            zoom: None,
            canvas: [1.0, 1.0],
            view_gen: 0,
            encoding: false,
            develop: Develop::default(),
            quality: PreviewQuality::stored(),
            drafting: false,
            frame_cost: 0.0,
            auto_draft: 0.75,
            refine: settings::flag(settings::REFINE_IDLE, true),
            camera_match: None,
            camera_curve: None,
            camera_jpeg: None,
            camera_job: None,
            saved: None,
            looks_loading: false,
            camera_pending: false,
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
                tab: "photo".into(),
                canvas: "empty",
                matching: "empty",
                export: ExportState { format: Some(format), quality, ..Default::default() },
                ..Default::default()
            },
            prefs: Prefs::load(),
            next_roll_id: 0,
            published: String::new(),
            frames: Default::default(),
            frame_count: 0,
            frames_dirty: false,
            curve_cache: Default::default(),
            timers: Timers::new(tx.clone()),
            tx,
            queue: VecDeque::new(),
            toasts: Vec::new(),
            dialogs: Vec::new(),
            actions: HashMap::new(),
            answers: HashMap::new(),
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
        // Files given on the command line (a file manager's "Open With"): added, the first opened.
        let files: Vec<PathBuf> = std::env::args_os()
            .skip(1)
            .filter(|a| !a.as_encoded_bytes().starts_with(b"--"))
            .map(PathBuf::from)
            .filter(|p| p.is_file())
            .collect();
        if let Some(first) = files.first().cloned() {
            app.queue.push_back(Msg::RollAddPaths(files, Some(first)));
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

    /// What QML needs to hear: the state and the frames when they changed, and the toasts and dialogs to show.
    pub fn publish(&mut self) -> Published {
        let state = std::mem::take(&mut self.dirty)
            .then(|| crate::view::state(self).to_string())
            .filter(|s| *s != self.published)
            .inspect(|s| self.published.clone_from(s));
        let frames =
            std::mem::take(&mut self.frames_dirty).then(|| serde_json::to_string(&self.frames).unwrap_or_default());
        Published {
            state,
            frames,
            toasts: std::mem::take(&mut self.toasts),
            dialogs: std::mem::take(&mut self.dialogs),
        }
    }

    /// Stores a frame for `image://rv/<key>/<generation>`; the generation tells QML it changed.
    pub fn show_image(&mut self, key: String, pixels: Vec<u8>, width: u32, height: u32, half: bool) {
        crate::bridge::rv_set_image(&key, pixels, width, height, half);
        // One counter for every key: a key dropped and set again never repeats a URL QML has seen.
        self.frame_count += 1;
        self.frames.insert(key, self.frame_count);
        self.frames_dirty = true;
    }

    /// The frames whose keys start with `prefix`.
    fn frames_with(&self, prefix: &str) -> Vec<String> {
        self.frames.keys().filter(|k| k.starts_with(prefix)).cloned().collect()
    }

    pub fn drop_image(&mut self, key: &str) {
        crate::bridge::rv_drop(key);
        self.frames_dirty |= self.frames.remove(key).is_some();
    }

    /// The grade's curves for QML: their points, and samples drawn as the shader evaluates them (the core's interpolation).
    pub fn curves_view(&self) -> (serde_json::Value, serde_json::Value) {
        use rawvibes_core::grade::{is_periodic, sample_curve, sample_periodic};
        let mut cache = self.curve_cache.borrow_mut();
        if cache.as_ref().is_none_or(|c| c.0 != self.grade.curves) {
            let samples: Vec<Vec<f32>> = (self.grade.curves.iter().enumerate())
                .map(|(ch, c)| {
                    let mut out = vec![0.0f32; 128];
                    if is_periodic(ch) {
                        sample_periodic(c, &mut out)
                    } else {
                        sample_curve(c, &mut out)
                    }
                    out.iter().map(|v| (v * 1000.0).round() / 1000.0).collect()
                })
                .collect();
            let points = serde_json::to_value(&self.grade.curves).unwrap_or_default();
            *cache = Some((self.grade.curves.to_vec(), points, samples.into()));
        }
        let c = cache.as_ref().expect("filled above");
        (c.1.clone(), c.2.clone())
    }

    /// The Lab tab renders the Lab's chain; every other tab the photo.
    pub fn in_lab(&self) -> bool {
        self.ui.tab == "lab"
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

    /// Asks QML for a dialog (`open`, `openMany`, `save`, `folder`, `confirm`, `presetName`); its answer,
    /// sent back with the request's id, becomes the message `answer` makes of it (`None`: nothing to do).
    fn ask(
        &mut self,
        kind: &str,
        mut options: serde_json::Value,
        answer: impl FnOnce(DialogAnswer) -> Option<Msg> + 'static,
    ) {
        let id = self.next_action;
        self.next_action += 1;
        self.answers.insert(id, Box::new(answer));
        options["id"] = id.into();
        self.dialogs.push((kind.to_owned(), options.to_string()));
    }

    /// A debounce tick or a result some later request superseded: it changes nothing to show.
    fn is_stale(&self, msg: &Msg) -> bool {
        match msg {
            Msg::DevelopSettled => !(self.drafting && self.refine),
            Msg::ExportEstimateRun(g, ..) | Msg::ExportEstimate(g, _) => *g != self.export_gen,
            Msg::Loaded(g, ..) | Msg::SourceMeta(g, ..) => *g != self.load_gen,
            Msg::SidecarLooks(g, _) => *g != self.looks_gen,
            _ => false,
        }
    }

    fn dispatch(&mut self, msg: Msg) {
        if self.is_stale(&msg) {
            return;
        }
        // A frame changes only the frames: no state to rebuild (a thumbnail pass sends hundreds).
        let frame_only = matches!(
            msg,
            Msg::Gpu(Done::Graded { .. } | Done::Thumb { .. } | Done::RollFrame { .. } | Done::Lab { .. })
        );
        self.dirty |= !frame_only;
        match msg {
            Msg::Open => {
                let folder = settings::folder(settings::PHOTO_FOLDER);
                self.ask(
                    "openMany",
                    serde_json::json!({
                        "title": gettext("Open Photo"),
                        "filters": [photo_filter()],
                        "folder": folder,

                    }),
                    |a| Some(Msg::RollAddPaths(a.paths, None)),
                );
            }
            Msg::RollAddPaths(paths, open_at) => self.add_to_roll(paths, open_at),
            Msg::MergeHdr => self.ask(
                "openMany",
                serde_json::json!({
                    "title": gettext("Merge Brackets to HDR"),
                    "filters": [photo_filter()],
                    "folder": settings::folder(settings::PHOTO_FOLDER),

                }),
                |a| Some(Msg::MergeHdrRun(a.paths)),
            ),
            Msg::MergeHdrRun(mut paths) => {
                if paths.len() < 2 {
                    return self.toast(&gettext("Pick two or more RAWs of the same scene at different exposures"));
                }
                paths.sort();
                let first = &paths[0];
                let stem = stem_label(first).unwrap_or_default();
                let out = free_path(first.parent().unwrap_or(std::path::Path::new(".")), &stem, "dng", "-HDR");
                self.toast(&gettext!("Merging {} photos to HDR…", paths.len()));
                let s = self.tx.clone();
                std::thread::spawn(move || {
                    let refs: Vec<&std::path::Path> = paths.iter().map(PathBuf::as_path).collect();
                    s.send(Msg::MergeHdrDone(rawvibes_core::merge::merge_files(&refs, &out).map(|_| out)));
                });
            }
            Msg::MergeHdrDone(Ok(path)) => {
                self.toast(&gettext!("HDR merge saved: {}", file_label(&path)));
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
            Msg::RollTick => {
                let grades = self.roll_grades();
                if !self.roll.is_empty() && self.roll_sent.as_ref() != Some(&grades) {
                    self.send(Job::RenderRoll(grades.clone(), self.active));
                    self.roll_sent = Some(grades);
                }
                self.refresh_gallery();
            }
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
                // The open photo's last edits reach its sidecar before it may close.
                self.save_now();
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
            Msg::SaveSidecar => {
                self.record_edits();
                self.save_now();
            }
            Msg::WriteSidecar => self.save_now(),
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
                // The looks join what was read; edits made while they loaded stay unsaved, so they are written.
                let looks = self.sidecar_state().looks;
                if let Some(saved) = self.saved.as_mut() {
                    saved.looks = looks;
                }
                self.reset_history();
                self.schedule_save();
                self.batch_ready();
            }
            Msg::AsInCamera(part) => self.as_in_camera(part),
            Msg::CameraCurve(path, ..) if Some(&path) != self.path.as_ref() => {}
            Msg::CameraCurve(photo, jpeg, curve) => {
                self.camera_jpeg = jpeg.map(|j| (photo.clone(), j));
                match curve {
                    None => self.toast(&gettext("No camera JPEG to match the tone curve to")),
                    Some(curve) => {
                        let before = self.grade.clone();
                        let mut grade = self.grade.clone();
                        grade.curves[MASTER] = curve.clone();
                        self.camera_curve = Some((photo, curve));
                        self.apply_grade(grade);
                        self.undo_toast(&gettext("Tone curve matched to the camera JPEG"), move || {
                            Msg::RestoreGrade(Box::new(before.clone()))
                        });
                    }
                }
                self.camera_pending = false;
                self.schedule_save();
                self.batch_ready();
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
                let options = serde_json::json!({
                    "title": gettext!("Find the LUT “{}”", l.name),
                    "filters": [lut_filter(&gettext("LUTs"))],
                    "folder": settings::folder(settings::LUT_FOLDER),
                });
                self.ask("open", options, move |a| Some(Msg::CameraLutFound(photo, l, a.path()?)));
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
                if let Err(e) = settings::set_flag(settings::AS_IN_CAMERA, on) {
                    self.toast(&gettext!("Cannot save the setting: {}", e));
                }
                self.prefs = Prefs::load();
            }
            Msg::CanvasClick(p) => {
                if let Some(at) = self.image_point(p).filter(|_| self.ui.picker) {
                    let detail = Box::new(self.applied_develop().detail);
                    self.send(Job::SampleRaw { at, radius: 4, detail });
                }
            }
            Msg::DevelopSettled => {
                self.drafting = false;
                self.encode_photo();
                self.request_view();
            }
            Msg::AiReady => {
                self.encode_photo();
                self.request_view();
            }
            Msg::Refine(on) => {
                self.refine = on;
                if let Err(e) = settings::set_flag(settings::REFINE_IDLE, on) {
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
                if self.roll.iter().any(|it| it.path == path && it.small.is_some()) {
                    return;
                }
                let (s, profile) = (self.tx.clone(), self.profile);
                std::thread::spawn(move || s.send(flows::strip_copy(path, profile)));
            }
            Msg::RawRead(generation, path, Ok(raw)) => self.send(Job::LoadRaw(generation, path, raw)),
            Msg::RawRead(generation, path, Err(e)) => {
                self.input(Msg::RollLoaded(path.clone(), Err(e.clone())));
                self.input(Msg::Loaded(generation, path, Err(e)));
            }
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
                self.ask(
                    "open",
                    serde_json::json!({
                        "title": gettext("Choose Reference"),
                        "filters": [photo_filter()],
                        "folder": settings::folder(settings::PHOTO_FOLDER),

                    }),
                    |a| a.path().map(Msg::MatchReference),
                );
            }
            Msg::MatchReference(path) => {
                self.ui.matching = "busy";
                let s = self.tx.clone();
                std::thread::spawn(move || {
                    // References are display images (a still, a JPEG); RAWs are rendered neutrally.
                    let r =
                        Decoded::load_max(&path, Some(1024)).map(|d| d.encode(Profile::Standard, &Develop::default()));
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
                self.ask("presetName", serde_json::json!({ "name": suggestion,  }), |a| {
                    Some(Msg::PresetSaveAs(a.name))
                });
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
                    let body = gettext!(
                        "A preset named “{}” already exists. Replacing it overwrites its settings.",
                        preset.name
                    );
                    return self.ask(
                        "confirm",
                        serde_json::json!({
                            "title": gettext("Replace Preset?"),
                            "body": body,
                            "action": gettext("Replace"),

                        }),
                        move |_| Some(Msg::PresetWrite(preset, true, apply)),
                    );
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
            Msg::PresetImport => self.ask(
                "open",
                serde_json::json!({
                    "title": gettext("Import Preset"),
                    "filters": [preset_filter()],

                }),
                |a| a.path().map(Msg::PresetImported),
            ),
            Msg::PresetImported(p) => match Preset::load(&p) {
                Ok(preset) => self.input(Msg::PresetWrite(Box::new(preset), false, true)),
                Err(e) => self.toast(&gettext!("Not a valid preset: {}", e)),
            },
            Msg::PresetExport => {
                let stem = self.path.as_deref().and_then(stem_label).unwrap_or_else(|| "preset".into());
                self.ask(
                    "save",
                    serde_json::json!({
                        "title": gettext("Export Preset"),
                        "filters": [preset_filter()],
                        "name": preset::file_name(&stem),

                    }),
                    |a| a.path().map(Msg::PresetExportTo),
                );
            }
            Msg::PresetExportTo(path) => match self.current_preset(stem_label(&path).unwrap_or_default()).save(&path) {
                Ok(()) => self.toast(&gettext!("Preset saved: {}", path.display())),
                Err(e) => self.toast(&gettext!("Cannot save the preset: {}", e)),
            },
            Msg::DialogAnswer(id, a) => {
                let answer = self.answers.remove(&id).filter(|_| !a.cancelled);
                if let Some(msg) = answer.and_then(|f| f(a)) {
                    self.input(msg);
                }
            }
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
                        let stem = stem_label(p).unwrap_or_default();
                        format!("{stem}_{}_{size}", self.profile.tag())
                    }
                    (What::Grade, None) => format!("chart_{}_{size}", self.profile.tag()),
                };
                let label = format!("{} {size}³", format.badge());
                let options = serde_json::json!({
                    "title": gettext!("Export {}", label),
                    "filters": [format!("{label} (*.{})", format.ext())],
                    "name": format!("{stem}.{}", format.ext()),
                    "folder": settings::folder(settings::LUT_FOLDER),
                });
                self.ask("save", options, move |a| Some(Msg::ExportTo(format, size, what, a.path()?)));
            }
            Msg::ExportTo(format, size, what, path) => {
                let title = stem_label(&path).unwrap_or_default();
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
                e.roll = self
                    .roll
                    .iter()
                    .filter(|it| self.ui.roll_filter == 0 || shown(self.ui.roll_filter, it.rating, it.label))
                    .count();
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
            Msg::HdrWhite(hdr, w, pq) => {
                // Said once per change: whether the screen and driver gave the preview an HDR swapchain.
                match (hdr, pq) {
                    (false, _) => {
                        eprintln!("rawvibes-qt: HDR preview: the swapchain is SDR (no HDR screen, or HDR is off)")
                    }
                    (true, true) => eprintln!("rawvibes-qt: HDR preview: HDR10 swapchain, SDR white at {w} cd/m²"),
                    (true, false) => eprintln!("rawvibes-qt: HDR preview: scRGB swapchain, SDR white ×{w}"),
                }
                crate::hdr::set_surface(hdr, w, pq);
                self.sent = None;
                self.render();
            }
            Msg::HdrPreview(on) => {
                self.hdr_preview = on;
                if !on {
                    // Nothing more to convert or keep until it is on again.
                    self.drop_image("hdr");
                }
                let _ = settings::set_flag(settings::HDR_PREVIEW, on);
                self.render();
            }
            Msg::Headroom(stops) => {
                self.headroom = stops.clamp(1.0, 3.0);
                self.render();
                self.timers.debounce(Duration::from_millis(500), Msg::HeadroomSave);
            }
            Msg::HeadroomSave => {
                let _ = settings::set_value(settings::HDR_HEADROOM, &format!("{:.2}", self.headroom));
            }
            Msg::ExportEstimateRun(generation, w, h) => {
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
            Msg::ExportEstimate(_, n) => self.ui.export.estimate = Some(gettext!("about {}", format_size(n))),
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
            Msg::ChooseFolder => self.ask(
                "folder",
                serde_json::json!({
                    "title": gettext("LUT folder"),
                    "folder": settings::folder(settings::LUT_FOLDER),

                }),
                |a| a.path().map(Msg::Folder),
            ),
            Msg::Folder(dir) => {
                if let Err(e) = settings::set_folder(settings::LUT_FOLDER, &dir) {
                    self.toast(&gettext!("Cannot save the LUT folder: {}", e));
                }
                self.prefs = Prefs::load();
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
                self.timers.debounce(HOVER_REST, Msg::HoverSettled(over));
            }
            Msg::HoverSettled(over) => self.input(Msg::Preview(over)),
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
                if std::mem::take(&mut self.levelling) {
                } else if self.ui.picker {
                    self.ui.picker = false;
                } else if self.editing.is_some() {
                    self.input(Msg::CropCancel);
                } else if self.candidate.is_some() || self.preview_req.is_some() {
                    self.clear_selection();
                } else if self.browsing {
                    self.input(Msg::Browse(false));
                }
            }
            Msg::Profile(profile) if profile == self.profile && self.photo.as_ref().is_some_and(|p| p.1 == profile) => {
            }
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
                LabAdd::File => {
                    let options = serde_json::json!({
                        "title": gettext("Add LUT"),
                        "filters": [lut_filter(&gettext("LUTs (.cube, .vlt, HALD .png)"))],
                        "folder": settings::folder(settings::LUT_FOLDER),
                    });
                    self.ask("open", options, move |a| Some(Msg::LayerFile(which, a.path()?)));
                }
                LabAdd::Grade => self.push_layer(which, LabKind::Grade, Profile::Standard),
                LabAdd::Convert => self.push_layer(which, LabKind::Convert(Profile::Standard), Profile::Standard),
            },
            Msg::OpenInLab => {
                if self.selected.is_some() {
                    self.input(Msg::Tab("lab".into()));
                }
                self.input(Msg::LayerAdd(Layers::Lab, LabAdd::Selected));
            }
            Msg::LayerFile(which, p) => self.load_layer(which, p, None),
            Msg::FlashDone => self.ui.flash_look = None,
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
                let n = self.layers_mut(which).len();
                if let Some(j) = j.filter(|&j| i < n && j < n) {
                    self.layers_mut(which).swap(i, j);
                    self.layers_changed(which, true);
                }
            }
            Msg::LayerIntensity(which, i, v) => {
                self.edit_layer(which, i, |l| l.intensity = (v / 100.0).clamp(0.0, 1.0) as f32)
            }
            Msg::LayerProfile(which, i, p) => self.edit_layer(which, i, |l| match &mut l.kind {
                LabKind::Convert(to) => *to = p,
                _ => l.input = p,
            }),
            Msg::LabInput(p) => self.lab_input = p,
            Msg::LookBlend(b) => {
                self.look_blend = b;
                self.show_looks();
                self.refresh_gallery();
            }
            Msg::LayerOn(which, i, on) => self.edit_layer(which, i, |l| l.on = on),
            Msg::FolderChanged => {
                self.timers.debounce(Duration::from_millis(500), Msg::FolderSettled);
            }
            Msg::FolderSettled => match settings::folder(settings::LUT_FOLDER) {
                Some(dir) => self.input(Msg::Folder(dir)),
                None => self.toast(&gettext("The LUT folder was moved or deleted: press F5 to choose it again")),
            },
            Msg::LabBlend(b) => {
                self.lab_blend = b;
                self.render_lab();
            }
            Msg::Tab(tab) => {
                let was_lab = self.in_lab();
                self.ui.tab = tab;
                if self.in_lab() != was_lab {
                    if self.in_lab() {
                        self.set_zoom(None);
                    }
                    self.refresh();
                }
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
                (self.editing, self.levelling) = (None, false);
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
            Msg::CropPress(p, tol) => {
                let Some((w, h)) = self.size() else { return };
                // On the crop: a handle or a move. Elsewhere: a new crop drawn from here.
                // A crop filling the photo cannot move: a drag on it draws one too.
                let handle = crop::hit(&self.crop, p, tol)
                    .filter(|&k| k != crop::Handle::Move || crop::movable(&self.crop, w, h));
                self.crop_grab = Some((handle, self.crop, p));
            }
            Msg::CropDrag(d) => {
                let (Some((handle, start, p)), Some((w, h))) = (self.crop_grab, self.size()) else { return };
                let aspect = self.ratio.aspect(w, h);
                let c = match handle {
                    Some(handle) => Some(crop::drag(start, handle, d, aspect, w, h)),
                    None => crop::draw(start, p, [p[0] + d[0], p[1] + d[1]], aspect, w, h),
                };
                if let Some(c) = c {
                    self.crop.rect = c.rect;
                    self.crop_changed();
                }
            }
            Msg::Level(on) => {
                self.levelling = on && self.decoded.is_some();
                if self.levelling {
                    self.input(Msg::CropEdit(true));
                }
            }
            Msg::CropLevel(dx, dy) => {
                self.levelling = false;
                if dx.hypot(dy) >= 10.0 {
                    let a = self.crop.angle + level_turn(dx, dy);
                    self.input(Msg::CropAngle(a));
                }
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
                self.prefs = Prefs::load();
            }
            Msg::MetaPolicy(tag) => {
                let _ = settings::set_value(settings::METADATA_POLICY, &tag);
                self.prefs = Prefs::load();
            }
            Msg::ExportSuffix(s) => {
                let _ = settings::set_value(settings::EXPORT_SUFFIX, &s);
                self.prefs = Prefs::load();
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
                self.prefs = Prefs::load();
            }
        }
    }
}

/// Degrees to add to the straighten angle so a line drawn on the shown frame
/// (screen y down) becomes level, or plumb when it is nearer vertical.
fn level_turn(dx: f32, dy: f32) -> f32 {
    let t = dy.atan2(dx).to_degrees();
    t - (t / 90.0).round() * 90.0
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
    settings::value(settings::HDR_HEADROOM).and_then(|q| q.parse().ok()).unwrap_or(export::HDR_HEADROOM).clamp(1.0, 3.0)
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
    use rawvibes_core::lut::Format;
    let exts = [Format::Cube, Format::Vlt, Format::Hald].map(Format::ext);
    let globs: Vec<String> =
        exts.iter().map(|e| format!("*.{e}")).chain(exts.iter().map(|e| format!("*.{}", e.to_uppercase()))).collect();
    format!("{label} ({})", globs.join(" "))
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

#[cfg(test)]
mod tests {
    use super::level_turn;

    #[test]
    fn level_turn_picks_the_nearest_axis() {
        let near = |a: f32, b: f32| (a - b).abs() < 1e-4;
        assert!(near(level_turn(100.0, 0.0), 0.0));
        assert!(near(level_turn(-100.0, 0.0), 0.0));
        assert!(near(level_turn(100.0, 100.0 * 5f32.to_radians().tan()), 5.0));
        assert!(near(level_turn(-100.0, -100.0 * 5f32.to_radians().tan()), 5.0));
        assert!(near(level_turn(100.0 * 5f32.to_radians().tan(), 100.0), -5.0));
    }
}
