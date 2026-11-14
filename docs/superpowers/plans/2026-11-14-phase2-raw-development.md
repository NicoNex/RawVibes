# Phase 2 — RAW development and "as in camera" Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** RAWs develop on the GPU — rescale, inpaint-opposed highlight
reconstruction, RCD demosaic, white balance (as shot, presets, picker,
Kelvin/tint), camera matrix into linear Rec.2020, RAW exposure — with a
preview whose fluidity/quality balance is a setting; every section that
has a camera equivalent gets "As in camera", plus a global button and an
"open RAWs as in camera" preference; edits persist in a sidecar.

**Architecture:** The CPU only decodes the file (rawler) and uploads the
raw mosaic (u16) once. On the GPU worker a load pass runs rescale →
as-shot WB → highlight reconstruction → demosaic, and keeps the result as
full-resolution *camera RGB* (as-shot balanced) in f16 plus a box-filtered
mip chain (`GpuRaw`). Every preview, zoomed view, thumbnail, filmstrip
frame and export then runs a *develop* pass: sample the visible region at
output size from the right mip → WB gains relative to as-shot → camera
matrix to linear Rec.2020 → exposure → encode to the selected input
profile. Its output is a `GpuPhoto`, which the existing grade shader
consumes unchanged, so Grade, looks, scopes and LUT export stay as they
are. Rasters (JPEG/PNG/TIFF) keep the phase-1 CPU path.

**Tech Stack:** Rust 2024, wgpu 30 (WGSL compute), rawler 0.8,
relm4 0.11 / GTK4 / libadwaita, serde_json.

**Spec:** `docs/superpowers/specs/2026-11-14-rawvalley-design.md`
(Goals → RAW development, "As in camera"; Architecture → Image pipeline,
LUT boundary, Resolution and fluidity, Algorithms, Persistence, UI).

## Decisions (author, 2026-11-14)

- Everything that makes sense on the GPU runs there (above). X-Trans and
  4-colour sensors are demosaiced by rawler on the CPU, then uploaded as
  camera RGB and continue on the GPU.
- Working space: **linear Rec.2020** for development (wide enough for
  camera colours, the spec's choice, and what phase-3 spatial tools will
  run in). At the encode, Rec.2020 → the profile's gamut (V-Gamut for
  V-Log, Rec.709 for Standard); out-of-gamut colours are compressed
  towards the achromatic axis (hue kept), never clipped per channel.
- "As in camera" per section (White balance, Exposure, Tone, Colour) plus
  a global header button, plus a preference that applies it when a RAW
  without a sidecar opens. Phase-3 sections add theirs where a camera
  setting exists.
- Preview quality is a preference: **Fast / Balanced (default) / Best**
  (see Task 5).
- Develop gets "White balance" and "Exposure"; the Grade's white balance
  section is renamed **"Warmth"**; group headings "Develop · not in LUT" /
  "Grade · in LUT" separate them.

## Global Constraints

- English strings, Italian `msgstr` in `po/it.po` in the same commit (`make pot`, then fill).
- `make check` passes at every commit; commit trailer as in phase 1.
- Edits are non-destructive: the RAW/JPEG is never written.
- Laggy sliders are an anti-goal: WB/exposure drags must stay fluid on a 24 MP RAW.
- Exported LUTs contain Grade + looks only; Develop never enters a LUT.
- GPU tests skip with "no GPU, skipped" when `Gpu::new()` fails, as the existing ones do.
- Code review + simplify once, at the end of the phase (not per task).
- Temporary GUI test hooks are marked `// TEMP-TEST` and removed before each commit.

## UX bar

- WB/exposure drags update the photo every frame at the draft resolution
  of the chosen quality, and the full-quality frame follows within
  ~150 ms of the last change.
- The picker is a toggle (crosshair cursor, Escape cancels); one click
  sets WB and switches it off.
- The WB preset dropdown shows "Custom" as soon as a slider moves and
  "As shot" whenever the gains equal the camera's.
- "As in camera" buttons sit in each section header next to the eye, same
  icon (`camera-photo-symbolic`), tooltip "As in camera"; insensitive for
  rasters.
- Sections not in the LUT carry a small "not in LUT" badge.
- Every new control has a tooltip and an accessible label.

## Review Focus

1. A RAW with no colour matrix or NaN `wb_coeffs` (unknown camera): opens
   with neutral as-shot gains; WB controls work, nothing is NaN.
2. Picker on a clipped or black area: no NaN/inf gains; toast "Pick a grey
   area that is not too dark or too bright"; WB unchanged.
3. Sidecar unreadable, newer, or naming a missing LUT: the photo opens with
   what could be read (missing LUTs skipped) and a toast; a sidecar that
   failed to load is not overwritten until the user edits.
4. A RAW larger than the GPU's `max_storage_buffer_binding_size` /
   `max_buffer_size` (e.g. 100 MP medium format): loads in tiles or fails
   with a clear toast, never a wgpu panic.
5. Portrait RAW (EXIF 6/8) with a crop and zoom: the picker samples the
   pixel under the cursor, and zoomed views show the same pixels as the CPU
   path did in phase 1.

---

### Task 1: Develop model and working space (CPU reference)

**Files:**
- Create: `core/src/develop.rs`
- Modify: `core/src/profiles.rs` (`Profile::from_rec2020()`), `core/src/lib.rs`

**Interfaces:**
- Produces:
  - `#[derive(Clone, Copy, Debug, PartialEq, Default, Serialize, Deserialize)] pub struct Develop { pub wb: Option<[f32; 2]>, pub exposure: f32 }` — `wb: None` = as shot, else `[kelvin, tint]`; exposure in EV.
  - `pub struct Camera { pub as_shot: [f32; 3], pub cam2rec2020: [[f32; 3]; 3], pub baseline_ev: f32, xyz2cam: [[f32; 3]; 3] }` with `Camera::from_raw(&rawler::RawImage) -> Camera` (same matrix derivation `Calibration::new` uses today, target Rec.2020; unknown camera → identity-ish, as-shot = 1), `Camera::neutral() -> Camera`.
  - `impl Camera { pub fn gains(&self, kelvin: f32, tint: f32) -> [f32; 3]; pub fn kelvin_tint(&self, gains: [f32; 3]) -> [f32; 2]; pub fn as_shot_kelvin_tint(&self) -> [f32; 2]; pub fn neutral_for(&self, cam: [f32; 3]) -> Option<[f32; 2]> }`
  - `pub fn develop_pixel(cam: [f32; 3], camera: &Camera, dev: &Develop, profile: Profile) -> [f32; 3]` — the CPU reference the WGSL develop pass must match (gains → `cam2rec2020` → × 2^(exposure) × `RAW_TO_SCENE` → gamut map → profile encode).
  - `pub fn gamut_compress(rgb: [f32; 3]) -> [f32; 3]` — moves a colour with a negative channel towards its luminance (Rec.709 Y weights in the target space) until min = 0; leaves in-gamut colours untouched.
  - `pub const WB_PRESETS: [(&str, f32, f32); 6]` = Daylight 5500/0, Cloudy 6500/10, Shade 7500/10, Tungsten 2850/0, Fluorescent 3800/20, Flash 5500/0.
- Kelvin → xy: CIE daylight locus for T ≥ 4000 K, Kim et al. Planckian approximation below; tint = Δuv along the locus normal (`tint / 3000`); XYZ (Y = 1) → camera via `xyz2cam`; multipliers = 1 / camera normalised to G, relative to `as_shot`. `kelvin_tint` inverts by bisection on R/B over 2000–15000 K, then solves tint from G. `neutral_for` → `None` if any channel ≤ 1e-4 or ≥ 0.98.
- `baseline_ev`: DNG BaselineExposure when present, else 0 (used by Exposure "As in camera").

- [ ] **Step 1: Failing tests**

```rust
#[test] fn kelvin_tint_round_trips()     // T in [2500, 3200, 4500, 5500, 6500, 9000] × tint in [-30, 0, 30], S9-like matrix:
                                         // kelvin_tint(gains(T, tint)) within 1% of T, 0.5 of tint
#[test] fn as_shot_gains_are_one()       // gains(as_shot_kelvin_tint()) ≈ [1, 1, 1] within 1e-3
#[test] fn neutral_for_makes_it_grey()   // gains(neutral_for(c)) ⊙ c has equal channels within 1e-3
#[test] fn neutral_for_rejects_clipped_and_black() // [0.99, 0.5, 0.5], [0.0, 0.1, 0.1] → None
#[test] fn gamut_compress_keeps_in_gamut_and_hue() // in-gamut unchanged; [-0.1, 0.5, 0.8] → min 0, same Y, hue angle within 2°
#[test] fn rec2020_standard_matches_phase1_on_rec709_colours() // develop_pixel of colours inside Rec.709 equals the
                                         // phase-1 Calibration + encode_scene result within 2e-4
```

- [ ] **Step 2:** `cargo test -p rawvalley-core develop` → FAIL (module missing).
- [ ] **Step 3:** Implement; `Profile::from_rec2020()` = `from_rec709() · M(2020→709)`.
- [ ] **Step 4:** `cargo test -p rawvalley-core develop` → PASS; `make check`.
- [ ] **Step 5:** Commit `Develop model: white balance, exposure, Rec.2020 working space`.

### Task 2: GPU RAW load — rescale, WB, highlight reconstruction

**Files:**
- Create: `core/src/raw.wgsl`, `core/src/raw.rs` (host side: `GpuRaw`, CFA upload, passes)
- Modify: `core/src/gpu.rs` (pipelines registered in `Gpu::new`), `core/src/photo.rs` (`pub struct RawData { pub mosaic: Arc<Vec<u16>>, width, height, cfa: [[u8; 6]; 6], cfa_size: [u8; 2], black: [f32; 4], white: f32, crop: [u32; 4], orientation: Orientation, camera: Camera }` and `pub fn read_raw(path) -> Result<RawData, String>` using rawler decode + metadata, no CPU develop)

**Interfaces:**
- Consumes: `Camera` (Task 1).
- Produces: `Gpu::load_raw(&self, raw: &RawData) -> Result<GpuRaw, String>` running, as compute passes: (1) rescale `(v − black[c]) / (white − black[c])` and as-shot WB into an f32 CFA buffer; (2) inpaint opposed: clipped mask (≥ 0.99 × clip[c], clip[c] = as-shot gain), per-colour chroma offset = mean over unclipped pixels on the 3×3-dilated mask border of (value − mean of the two opposed colours) via a workgroup reduction + atomic accumulation, then clipped value = max(value, opposed mean + offset). Generic CFA pattern (Bayer and X-Trans) through the `cfa` uniform.
- Buffer limits: when the CFA exceeds `max_storage_buffer_binding_size`, process in horizontal bands with an 8-row overlap (Review Focus 4); if a single row band still does not fit, `Err("photo too large for this GPU")`.

- [ ] **Step 1: Failing GPU tests in `raw.rs`**

```rust
#[test] fn rescale_maps_black_and_white() // mosaic of black → 0, white → 1 × as-shot gain, per CFA colour
#[test] fn unclipped_pixels_are_untouched() // random CFA < 0.9 · clip: after inpaint == after rescale, bit for bit
#[test] fn clipped_green_is_rebuilt_above_clip() // warm blob with G clipped: rebuilt G ≥ clip, R/G within 5% of the
                                                  // border's R/G (no magenta)
#[test] fn fully_clipped_area_stays_white() // all channels clipped: ≥ clip, channels equal within 2%
#[test] fn xtrans_pattern_is_honoured()     // same as unclipped_pixels_are_untouched with a 6×6 X-Trans pattern
```

- [ ] **Step 2:** `cargo test -p rawvalley-core raw::` → FAIL.
- [ ] **Step 3:** Implement `read_raw`, the WGSL passes and `load_raw` up to the reconstructed CFA.
- [ ] **Step 4:** Tests PASS; `make check`.
- [ ] **Step 5:** Commit `GPU RAW load: rescale, white balance, highlight reconstruction`.

### Task 3: GPU RCD demosaic and the camera-RGB mip chain

**Files:**
- Modify: `core/src/raw.wgsl`, `core/src/raw.rs`

**Interfaces:**
- Consumes: the reconstructed CFA (Task 2).
- Produces: `GpuRaw { levels: Vec<(wgpu::Buffer, u32, u32)>, camera: Camera, orientation: Orientation, crop: [u32; 4] }` — level 0 = full-res cropped camera RGB packed f16 (`pack2x16float`, 8 bytes/pixel), each next level a 2× box downscale until the longer side ≤ 256.
- RCD v2.3 (Luis Sanz Rodríguez; RawTherapee `rcd_demosaic.cc`) as passes: (1) V/H directional discrimination, (2) low-pass filter, (3) green at R/B, (4) P/Q diagonal discrimination, (5) R/B at B/R, (6) R/B at G; 4-pixel border bilinear. X-Trans/4-colour: rawler's CPU demosaic in `read_raw`, uploaded straight into level 0.
- Orientation and crop are applied when sampling (Task 4), not by copying.

- [ ] **Step 1: Failing GPU tests**

```rust
#[test] fn flat_field_is_exact()        // constant (0.3, 0.5, 0.2) CFA → every pixel within 1e-3 (f16)
#[test] fn smooth_gradient_is_close()   // per-channel linear ramps → max error < 3e-3 away from the border
#[test] fn sharp_edge_has_no_zipper()   // vertical black/white edge: |G − R| < 0.05 at 3 px either side
#[test] fn every_bayer_phase_works()    // RGGB, BGGR, GRBG, GBRG pass flat_field_is_exact
#[test] fn mips_preserve_the_mean()     // each level's mean equals level 0's within 1e-3
#[test] #[ignore] fn open_time()        // RAWVALLEY_RAW: read_raw + load_raw on the sample, prints the time
```

- [ ] **Step 2:** FAIL; **Step 3:** implement; **Step 4:** PASS, `make check`, run `open_time` in release and note it in the ledger.
- [ ] **Step 5:** Commit `GPU RCD demosaic and camera-RGB mips`.

### Task 4: GPU develop pass replaces the CPU RAW path

**Files:**
- Modify: `core/src/raw.wgsl` (develop entry point), `core/src/raw.rs`, `core/src/gpu.rs` (jobs, worker), `core/src/photo.rs` (`Decoded::Raw` holds `Arc<RawData>`; the CPU `calibrated`/`Calibration` path is deleted), `gtk/src/main.rs` (RAW photos go through the new jobs)

**Interfaces:**
- Consumes: `GpuRaw` (Task 3), `develop_pixel` as the reference (Task 1).
- Produces:
  - `Gpu::develop(&self, raw: &GpuRaw, r: &Region, w: u32, h: u32, dev: &Develop, profile: Profile) -> GpuPhoto` — picks the finest mip whose size ≥ the output scale, bilinear sampling through orientation + crop + `Region`, then the per-pixel develop; writes the `GpuPhoto` the grade shader reads.
  - `Gpu::read_photo(&self, p: &GpuPhoto) -> Photo` (readback, for match sources and CPU consumers).
  - Jobs: `Job::SetRaw(Arc<RawData>)` (replaces `SetPhoto` for RAWs; loads `GpuRaw`, frees the previous one), `Job::SetDevelop { develop: Develop, profile: Profile }`, `Job::SetRawView { region: Region, w: u32, h: u32, draft: bool }` (replaces `SetView` + CPU encode for RAWs); thumbnails develop the whole frame at `THUMB_MAX`.
  - Filmstrip RAWs: `read_raw` → `load_raw` → develop the whole frame at the strip size → keep only that small `GpuPhoto` + `Camera` (full-res buffers dropped), so N filmstrip RAWs cost N small buffers.
  - Export (`Job::Export` of the photo, phase 5) will call `develop` at full resolution in tiles.

- [ ] **Step 1: Failing GPU test** `develop_matches_cpu_reference`: a synthetic `GpuRaw` (flat camera-RGB patches) developed with several `Develop`/`Profile` combinations equals `develop_pixel` within 2e-3 per channel; `develop_honours_orientation_and_crop`: a 4×2 marked image with orientation 6 and a crop samples the expected pixels.
- [ ] **Step 2:** FAIL; **Step 3:** implement the pass and the jobs; switch `gtk` RAW loading, zoom, thumbnails, filmstrip and match source to them; delete the CPU RAW path.
- [ ] **Step 4:** GUI check with `// TEMP-TEST` hooks on the S9 sample: fit, 100% zoom, pan, before/after, filmstrip of 3 RAWs; compare the fit view with the phase-1 capture (same framing, RMSE < 0.03 — the demosaic and highlights changed). Remove hooks; `make check`.
- [ ] **Step 5:** Commit `RAW preview developed on the GPU`.

### Task 5: Preview quality preference

**Files:**
- Modify: `gtk/src/main.rs` (Preferences row, draft/settle logic), `gtk/src/settings.rs` (`PREVIEW_QUALITY` key), `po/it.po`

**Interfaces:**
- Consumes: `Job::SetRawView { draft }` (Task 4).
- Produces: `enum PreviewQuality { Fast, Balanced, Best }` stored as "fast"/"balanced"/"best". While a Develop or Grade slider is being dragged, the RAW view renders at the draft scale of the canvas' device pixels — Fast 0.5, Balanced 0.75, Best 1.0 — and 150 ms after the last change at 1.0. Best also lifts the fitted preview's `PREVIEW_MAX` cap to the canvas' device-pixel size (HiDPI). Preferences row: "Preview while editing" with subtitle "Faster previews are softer while you drag; the full-quality frame follows when you stop."

- [ ] **Step 1:** Implement; Preferences combo row; setting remembered.
- [ ] **Step 2:** GUI check: drag Temperature for 2 s on the S9 sample in each mode, log frame times with a `// TEMP-TEST` counter; Balanced keeps ≥ 30 fps on this machine; the settled frame equals the Best frame (RMSE 0). Remove hooks.
- [ ] **Step 3:** Italian strings; `make check`; commit `Preview quality preference`.

### Task 6: Develop group in the sidebar

**Files:**
- Modify: `gtk/src/main.rs` (`build_sidebar`, `App.develop: Develop`, messages, picker), `po/it.po`

**Interfaces:**
- Consumes: `Develop`, `Camera`, `WB_PRESETS` (Task 1); `Job::SetDevelop` (Task 4).
- Produces: `Msg::Develop(DevelopEdit)` with `enum DevelopEdit { AsShot, Preset(usize), Kelvin(f32), Tint(f32), Exposure(f32), CameraExposure, Pick(bool), Picked([f32; 2]) }`; `fn not_in_lut_badge() -> gtk::Label`; `fn camera_button(tip: &str, msg: Msg) -> gtk::Button` (the per-section "As in camera" button, used here and in Task 7).
- Layout: Scopes · **Develop · not in LUT** (White balance: preset dropdown [As shot, Daylight…, Custom], picker toggle, Temperature 2000–15000 K, Tint −100…100, header camera button = `AsShot`; Exposure: −3…+3 EV step 0.05, header camera button = `CameraExposure` → `baseline_ev`) · **Grade · in LUT** (Input profile, Reference match, Warmth [renamed], Tone, …, LUTs) · **Finish** (Crop with badge) · Reset all (resets Develop too, existing undo toast). Develop hidden for rasters.
- Picker: while active, a canvas click maps through zoom, crop and orientation to image coordinates (reuse `Region::point`), `Job::SampleRaw { at: [f32; 2], radius: 4 }` → `Done::Sampled([f32; 3])` (mean camera RGB from level 0) → `camera.neutral_for`; `None` → Review Focus 2 toast.

- [ ] **Step 1:** Headings, rename to "Warmth", Develop sections wired; `make check`.
- [ ] **Step 2:** Picker job + UI.
- [ ] **Step 3:** GUI check (`// TEMP-TEST`): Tungsten then As shot returns the as-shot image exactly (RMSE 0); picker on the S9 sample's grey building gives R≈G≈B there; portrait RAW picks the right pixel (Review Focus 5). Remove hooks.
- [ ] **Step 4:** Italian strings; `make check`; commit `Develop group: white balance and RAW exposure`.

### Task 7: "As in camera" — per section, global, on open

**Files:**
- Modify: `core/src/photo.rs` (`pub fn embedded_preview(path: &Path) -> Result<Photo, String>` via `rawler::analyze::extract_preview_pixels`, fitted to 1024), `core/src/matching.rs` (`impl Match { pub fn lightness_only(&self) -> Match; pub fn colour_only(&self) -> Match }`), `gtk/src/main.rs`, `gtk/src/settings.rs` (`AS_IN_CAMERA` key), `po/it.po`

**Interfaces:**
- Consumes: `request_match` (existing), `camera_button`, `DevelopEdit::{AsShot, CameraExposure}` (Task 6).
- Behaviour:
  - Tone header camera button → camera match, lightness part only; Colour header → colour (a/b) part only; Reference match gets a "Camera JPEG" button (both parts). The match runs once per photo against its embedded JPEG (source: the photo developed as shot, before the grade) and is cached per path (`App.camera_match: Option<(PathBuf, Match)>`).
  - Header "As in camera" = WB as shot + camera exposure + full camera match.
  - Preferences switch "Open RAWs as in camera" (off by default): applied when a RAW without a sidecar becomes active.
  - No embedded preview → toast "This RAW has no camera JPEG to match"; all camera buttons insensitive for rasters.
- `lightness_only` keeps the lightness curve and sets `ab` to the identity transfer; `colour_only` keeps `ab` and sets the lightness curve to `identity_curve()`.

- [ ] **Step 1: Failing tests** `match_parts_split` (`lightness_only(m).ab` == identity, `colour_only(m).lightness` == `identity_curve()`, the other part unchanged); `embedded_preview_of_sample_raw` (`RAWVALLEY_RAW`, skips without it: longer side 640–1024, finite pixels in 0..1).
- [ ] **Step 2:** FAIL; implement; PASS.
- [ ] **Step 3:** Wire buttons and the preference; GUI check: global button on the S9 sample brings the histogram close to the embedded JPEG's; Tone-only changes lightness but not chroma (vectorscope unchanged); with the preference on, opening the RAW starts matched.
- [ ] **Step 4:** Italian strings; `make check`; commit `As in camera per section, global and on open`.

### Task 8: Sidecar persistence

**Files:**
- Create: `core/src/sidecar.rs`
- Modify: `core/src/preset.rs` (factor `pub(crate) fn grade_to_json` / `grade_from_json` out of `Preset`), `gtk/src/main.rs`, `po/it.po`

**Interfaces:**
- Produces:
  - `pub struct Sidecar { pub develop: Develop, pub grade: Grade, pub crop: Crop, pub ratio: Ratio, pub looks: Vec<LookRef>, pub look_blend: Blend }`, `pub struct LookRef { pub path: PathBuf, pub intensity: f32, pub input: Profile, pub on: bool }`.
  - `pub fn path_for(photo: &Path) -> PathBuf` → `<file>.rawvalley.json`.
  - `Sidecar::load(photo: &Path) -> Result<Option<Sidecar>, String>` (`Ok(None)` when absent); `Sidecar::save(&self, photo: &Path) -> Result<(), String>` (write `<name>.tmp`, then rename).
  - JSON `{"format":"rawvalley-sidecar","version":1, …}`; grade validated by `grade_from_json` (NaN, broken curves rejected as for presets).
- Semantics: the active photo's Develop and crop load from its sidecar; Grade, looks and blend are shared by the roll and load from the first photo opened into an empty roll. Saves the active photo's sidecar 1 s after the last edit. Missing LUT files skipped with one toast; write failures toast once per session.

- [ ] **Step 1: Failing tests**

```rust
#[test] fn sidecar_round_trips()             // save then load (tempdir) → equal
#[test] fn sidecar_path_is_next_to_the_file() // "/a/P1.RW2" → "/a/P1.RW2.rawvalley.json"
#[test] fn absent_sidecar_is_none()
#[test] fn corrupt_or_newer_sidecar_is_an_error() // "{" and version 99 → Err
#[test] fn nan_in_sidecar_is_rejected()       // develop.exposure NaN → Err
```

- [ ] **Step 2:** FAIL; implement; PASS.
- [ ] **Step 3:** Wire load/save (Review Focus 3); GUI check: edit WB, crop, add a look, quit, reopen → same image (RMSE 0).
- [ ] **Step 4:** Italian strings; `make check`; commit `Sidecar persistence next to the photo`.

### Phase close

- [ ] `/code-review high` then `/simplify` on `master...HEAD`, fix, `make check`, commit; merge to `master` after the author's OK.
