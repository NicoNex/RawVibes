# Phase 2 — RAW development and "as in camera" Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** RAWs develop with RCD demosaic, inpaint-opposed highlight
reconstruction, adjustable white balance (as shot, presets, picker,
Kelvin/tint) and RAW exposure; one button starts the photo "as in camera"
from the embedded JPEG; edits persist in a sidecar next to the file.

**Architecture:** Loading a RAW runs, once, on the CPU: rescale → as-shot
WB → highlight reconstruction → demosaic → crop → orient, and keeps the
result as *camera RGB* (as-shot balanced, no matrix) in the existing
`Pyramid`. Everything adjustable is per-pixel and runs when a region is
encoded (`Decoded::encode_*`): WB gains relative to as-shot → camera matrix
→ exposure → profile encode. So a WB or exposure drag costs what a profile
change costs today (one preview-sized encode), and the Grade, LUT export
and zoom code are untouched.

**Tech Stack:** Rust 2024, rawler 0.8, rayon, relm4 0.11 / GTK4 /
libadwaita, serde_json.

**Spec:** `docs/superpowers/specs/2026-11-14-rawvalley-design.md`
(Goals → RAW development, "As in camera"; Architecture → Image pipeline,
LUT boundary, Algorithms, Persistence, UI).

## Deviations from the spec (builder's calls — reject any)

1. **RCD and inpaint opposed run on the CPU (rayon), not in WGSL.** They run
   once per file open; CPU code is far easier to test against invariants.
   Move to WGSL in a later phase if open time on a 24 MP RW2 exceeds ~1.5 s.
2. **Full-resolution f16 residency on the GPU is deferred.** The phase-1 CPU
   pyramid stays; it already gives fluid zoom/pan.
3. **Working space stays linear Rec.709** (the whole grade/profile code
   assumes it). Negative values out of the camera matrix are kept until the
   profile encode, so wide-gamut colours are not clipped early.
4. **"As in camera" is one header button plus "Camera JPEG" in Reference
   match**, not one button per Tone/Colour section: the match engine fits
   lightness and colour together, so separate buttons would do the same.
5. **Author's decision (2026-11-14):** Develop gets "White balance" and
   "Exposure"; the Grade's white balance section is renamed **"Warmth"**
   (same sliders); group headings "Develop · not in LUT" / "Grade · in LUT"
   separate them.

## Global Constraints

- English strings, Italian `msgstr` in `po/it.po` in the same commit (`make pot`, then fill).
- `make check` passes at every commit; commit trailer as in phase 1.
- Edits are non-destructive: the RAW/JPEG is never written.
- Laggy sliders are an anti-goal: WB/exposure drags must stay fluid on a 24 MP RAW.
- Exported LUTs contain Grade + looks only; Develop never enters a LUT.
- Code review + simplify once, at the end of the phase (not per task).
- Temporary GUI test hooks are marked `// TEMP-TEST` and removed before each commit.

## UX bar

- WB and exposure sliders update the photo within one frame of the drag
  (encode off the main thread, latest wins — reuse `encode_photo`).
- The picker is a toggle (cursor becomes a crosshair, Escape cancels);
  one click sets WB and switches the picker off.
- The WB preset dropdown shows "Custom" as soon as a slider moves, and
  "As shot" whenever the gains equal the camera's.
- Sections not in the LUT carry a small "not in LUT" badge (Develop
  sections, Crop).
- Every new control has a tooltip and an accessible label.

## Review Focus

1. A RAW with no colour matrix or NaN `wb_coeffs` (unknown camera): opens
   with as-shot = neutral gains; WB controls work, nothing is NaN.
2. Picker on a clipped or black area: no NaN/inf gains; a toast says
   "Pick a grey area that is not too dark or too bright" and WB is unchanged.
3. Sidecar unreadable, from a newer version, or naming a missing LUT: the
   photo opens with what could be read (missing LUTs skipped) and a toast;
   a corrupt sidecar is not overwritten until the user edits.
4. Sidecar write fails (read-only folder): one toast per session, editing
   continues.
5. Portrait RAW (EXIF 6/8) with a crop: the picker samples the pixel under
   the cursor, not a rotated/mirrored one.

---

### Task 1: Camera model, `Develop` and per-pixel development

**Files:**
- Create: `core/src/develop.rs` (Kelvin/tint math, `Camera`, `Develop`)
- Modify: `core/src/photo.rs` (`Decoded::Raw` carries a `Camera`; `calibrated` stops applying the matrix; `encode_*` take `&Develop`)
- Modify: `core/src/lib.rs`, callers in `gtk/src/main.rs` (pass `&self.develop`), `core/src/gpu.rs` tests if they call `encode`

**Interfaces:**
- Produces:
  - `pub struct Develop { pub wb: Option<[f32; 2]>, pub exposure: f32 }` — `wb: None` = as shot, `Some([kelvin, tint])`; `exposure` in EV; `Default` = as shot, 0 EV; `serde` derive.
  - `pub struct Camera { pub as_shot: [f32; 3], pub cam2rgb: [[f32; 3]; 3], xyz2cam: [[f32; 3]; 3] }`
  - `impl Camera { pub fn gains(&self, kelvin: f32, tint: f32) -> [f32; 3]; pub fn kelvin_tint(&self, gains: [f32; 3]) -> [f32; 2]; pub fn as_shot_kelvin_tint(&self) -> [f32; 2]; pub fn neutral_for(&self, cam: [f32; 3]) -> Option<[f32; 2]>; pub fn rec709() -> Self }`
    (`gains` = multipliers relative to as-shot, G = 1; `neutral_for` = Kelvin/tint that makes that as-shot-balanced camera colour grey, `None` if any channel ≤ 1e-4 or ≥ 0.98).
  - `pub enum Decoded { Raster(Arc<Pyramid>), Raw(Arc<Pyramid>, Arc<Camera>) }`
  - `Decoded::encode_fit(&self, r, max, profile, dev: &Develop)`, `encode_region(.., dev)`, `encode(profile, dev)`; `Decoded::camera(&self) -> Option<&Camera>`; `Decoded::sample(&self, at: [f32; 2], radius: u32) -> [f32; 3]` (mean of the full-res pyramid around an image point).
  - `pub const WB_PRESETS: [(&str, f32, f32); 6]` = Daylight 5500/0, Cloudy 6500/10, Shade 7500/10, Tungsten 2850/0, Fluorescent 3800/20, Flash 5500/0.
- Kelvin → xy: CIE daylight locus for T ≥ 4000 K, Kim et al. Planckian approximation below; tint = Δuv along the locus normal, `tint / 3000`; XYZ (Y = 1) → camera via `xyz2cam`; multipliers = 1 / camera, normalised to G; relative to `as_shot`. `kelvin_tint` inverts by bisection on the R/B ratio over 2000–15000 K, then solves tint from G.
- 4-colour sensors: calibrated to Rec.709 at load as today, `Camera::rec709()` (identity matrix, as-shot = 1). `// shortcut: WB acts on Rec.709 for CYGM/RGBE sensors, upgrade if one is ever reported`.

- [ ] **Step 1: Failing tests in `develop.rs`**

```rust
#[test] fn kelvin_tint_round_trips() // for T in [2500, 3200, 4500, 5500, 6500, 9000], tint in [-30, 0, 30]:
    // cam.kelvin_tint(cam.gains(T, tint)) within 1% of T and 0.5 of tint (cam = a real S9-like matrix fixture)
#[test] fn as_shot_gains_are_one()       // cam.gains(as_shot_kelvin_tint) ≈ [1, 1, 1] within 1e-3
#[test] fn neutral_for_makes_it_grey()   // gains(neutral_for(c)) · c has equal channels within 1e-3
#[test] fn neutral_for_rejects_clipped_and_black() // [0.99, 0.5, 0.5] and [0.0, 0.1, 0.1] → None
#[test] fn default_develop_matches_phase1_output() // a synthetic camera-RGB pyramid encoded with Develop::default
    // equals the phase-1 Calibration output (same wb, matrix, RAW_TO_SCENE) within 1e-4
```

- [ ] **Step 2:** `cargo test -p rawvalley-core develop` → FAIL (module missing).
- [ ] **Step 3:** Implement `develop.rs`; change `load_raw`/`calibrated` to keep as-shot-balanced camera RGB and build `Camera` from the same matrix code `Calibration::new` uses; `encode_scene` applies `gains → cam2rgb → × 2^exposure × RAW_TO_SCENE` before the profile encode. Update every caller (grep `encode_fit\|encode_region\|\.encode(`).
- [ ] **Step 4:** `make check` → PASS; open the S9 sample: it looks as before.
- [ ] **Step 5:** Commit `Develop parameters: adjustable white balance and RAW exposure`.

### Task 2: Highlight reconstruction (inpaint opposed)

**Files:**
- Create: `core/src/highlights.rs`
- Modify: `core/src/photo.rs` (`load_raw` Bayer path: `RawDevelop::new_with(&[Rescale])` → `Intermediate::Monochrome` CFA → as-shot WB per CFA colour → `highlights::inpaint_opposed` → (Task 3 demosaic) → crop/orient; the fade-to-neutral in `Calibration::apply` goes)

**Interfaces:**
- Produces: `pub fn inpaint_opposed(cfa: &mut [f32], width: usize, height: usize, colour_at: impl Fn(usize, usize) -> usize + Sync, clip: [f32; 3])`
  (in place on the WB-balanced CFA; `clip[c]` = 1.0 × that colour's WB gain).
- Algorithm (darktable 4.x "inpaint opposed"): mark pixels ≥ 0.99 × clip; for each colour, the chrominance offset = mean over unclipped 3×3-dilated border pixels of (value − mean of the two opposed colours); clipped value = max(value, opposed mean + offset).

- [ ] **Step 1: Failing tests**

```rust
#[test] fn unclipped_pixels_are_untouched()     // random CFA below 0.9·clip: output == input bit for bit
#[test] fn clipped_green_is_rebuilt_above_clip() // a smooth warm blob with G clipped: rebuilt G ≥ clip, and
    // R/G of rebuilt pixels within 5% of the blob border's R/G (no magenta)
#[test] fn fully_clipped_area_stays_white()      // all channels clipped: values ≥ clip, channels equal within 2%
```

- [ ] **Step 2:** `cargo test -p rawvalley-core highlights` → FAIL.
- [ ] **Step 3:** Implement; wire into `load_raw` (X-Trans and 4-colour keep rawler's path, no reconstruction).
- [ ] **Step 4:** `make check`; S9 sky sample: no magenta, no band where the old 15% fade was.
- [ ] **Step 5:** Commit `Highlight reconstruction: inpaint opposed`.

### Task 3: RCD demosaic for Bayer sensors

**Files:**
- Create: `core/src/demosaic.rs`
- Modify: `core/src/photo.rs` (Bayer → `demosaic::rcd`; X-Trans → rawler's `Demosaic` step as today); crop with the same rects rawler's `CropActiveArea` + `CropDefault` use

**Interfaces:**
- Consumes: the reconstructed CFA from Task 2.
- Produces: `pub fn rcd(cfa: &[f32], width: usize, height: usize, colour_at: impl Fn(usize, usize) -> usize + Sync) -> Vec<[f32; 3]>`
- Algorithm: RCD v2.3 (Luis Sanz Rodríguez; as in RawTherapee `rcd_demosaic.cc`): V/H high-pass, P/Q diagonal discrimination, ratio-corrected low-pass green, then R/B at green and at opposite sites; 4-pixel border filled bilinearly. Rows processed in rayon bands with a 4-pixel overlap.

- [ ] **Step 1: Failing tests**

```rust
#[test] fn flat_field_is_exact()        // CFA of a constant colour (0.3, 0.5, 0.2) → every pixel equals it within 1e-5
#[test] fn smooth_gradient_is_close()   // linear ramp in x and y per channel → max error < 2e-3 away from the border
#[test] fn sharp_edge_has_no_zipper()   // vertical black/white edge: |G − R| < 0.05 on both sides 3 px from the edge
#[test] fn every_bayer_phase_works()    // RGGB, BGGR, GRBG, GBRG all pass flat_field_is_exact
```

- [ ] **Step 2:** `cargo test -p rawvalley-core demosaic` → FAIL.
- [ ] **Step 3:** Implement; replace rawler's PPG for Bayer.
- [ ] **Step 4:** `make check`; time `Decoded::load` on the S9 RW2 in release (`cargo test --release -p rawvalley-core -- --ignored open_time`, an `#[ignore]` test reading `RAWVALLEY_RAW`): print the time; note it in the ledger (> 1.5 s → deviation 1 is revisited).
- [ ] **Step 5:** Commit `RCD demosaic for Bayer sensors`.

### Task 4: Develop group in the sidebar

**Files:**
- Modify: `gtk/src/main.rs` (`build_sidebar`: group headings, Develop sections; `App.develop: Develop`; messages; picker on the canvas; re-encode on change)
- Modify: `po/it.po`

**Interfaces:**
- Consumes: `Develop`, `Camera`, `WB_PRESETS`, `Decoded::sample` (Task 1).
- Produces: `Msg::Develop(DevelopEdit)` with `enum DevelopEdit { AsShot, Preset(usize), Kelvin(f32), Tint(f32), Exposure(f32), Pick(bool), Picked([f32; 2]) }`; `fn not_in_lut_badge() -> gtk::Label` (reused by Crop).
- Layout, top to bottom: Scopes · **Develop · not in LUT** (White balance: preset dropdown [As shot, Daylight…, Custom], "As shot" button, picker toggle `color-select-symbolic`, Temperature 2000–15000 K, Tint −100…100; Exposure: −3…+3 EV, step 0.05) · **Grade · in LUT** (Input profile, Reference match, Warmth [renamed], Tone, …, LUTs) · **Finish** (Crop with badge) · Reset all. Develop group hidden for rasters.
- Changes re-encode through `encode_photo` (already off-thread, latest wins); the filmstrip keeps each photo's own develop (default as shot until Task 6 loads sidecars).
- "Reset all" resets Develop too (with the existing undo toast).

- [ ] **Step 1:** Add the group headings, rename the Grade section to "Warmth", add the Develop sections wired to `Msg::Develop`; `make check`.
- [ ] **Step 2:** Picker: while active, a click on the canvas maps the point through zoom, crop and orientation to image coordinates (reuse `Region::point`), calls `decoded.sample(at, 4)` then `camera.neutral_for`; `None` → the Review Focus 2 toast.
- [ ] **Step 3:** GUI check with `// TEMP-TEST` hooks: drag Temperature 3000 → 9000 K (photo warms smoothly, no stall); preset Tungsten then As shot returns the exact as-shot image (RMSE 0 vs the first capture); picker on the S9 sample's grey building gives near-equal R/G/B there; a portrait RAW picks the right pixel (Review Focus 5). Remove hooks.
- [ ] **Step 4:** Italian strings; `make check`; commit `Develop group: white balance and RAW exposure`.

### Task 5: "As in camera"

**Files:**
- Modify: `core/src/photo.rs` (`pub fn embedded_preview(path: &Path) -> Result<Photo, String>` via `rawler::analyze::extract_preview_pixels`, display-encoded RGBA, fitted to 1024)
- Modify: `gtk/src/main.rs` (header button "As in camera"; "Camera JPEG" button in Reference match)

**Interfaces:**
- Consumes: `request_match(reference: Arc<Photo>)` (existing), `DevelopEdit::AsShot`.
- Flow: header button → `Msg::Develop(DevelopEdit::AsShot)`, Develop exposure 0, then load the embedded preview off-thread and `request_match` with it (the match source is the photo as developed, before the grade, as today). "Camera JPEG" does only the match. No embedded preview → toast "This RAW has no camera JPEG to match". Both insensitive for rasters.

- [ ] **Step 1: Failing test** `embedded_preview_of_sample_raw` (`RAWVALLEY_RAW` env, skips without it): returns a photo whose longer side ≥ 640 and ≤ 1024, pixels finite in 0..1.
- [ ] **Step 2:** Implement `embedded_preview`; test passes with the S9 sample.
- [ ] **Step 3:** Wire both buttons; GUI check: on the S9 sample, "As in camera" makes the photo's histogram close to the embedded JPEG's (scopes capture), match amount 100.
- [ ] **Step 4:** Italian strings; `make check`; commit `As in camera: as-shot WB and a match to the camera JPEG`.

### Task 6: Sidecar persistence

**Files:**
- Create: `core/src/sidecar.rs`
- Modify: `gtk/src/main.rs` (load on open / when a photo becomes active; debounced save)

**Interfaces:**
- Produces:
  - `pub struct Sidecar { pub develop: Develop, pub grade: Grade, pub crop: Crop, pub ratio: Ratio, pub looks: Vec<LookRef> }`, `pub struct LookRef { pub path: PathBuf, pub intensity: f32, pub input: Profile, pub on: bool }`, plus `look_blend: Blend`.
  - `pub fn path_for(photo: &Path) -> PathBuf` → `<file>.rawvalley.json` (e.g. `P1010192.RW2.rawvalley.json`).
  - `Sidecar::load(photo: &Path) -> Result<Option<Sidecar>, String>` (`Ok(None)` when absent); `Sidecar::save(&self, photo: &Path) -> Result<(), String>` (write to `<name>.tmp`, then rename).
  - JSON: `{"format":"rawvalley-sidecar","version":1, ...}`; the grade part reuses `preset.rs`'s `File` validation (factor it into `pub(crate) fn grade_from_json` / `grade_to_json` there), so NaN or broken curves are rejected the same way.
- Semantics: the active photo's Develop and crop load from its sidecar; Grade, looks and look blend are shared by the roll, so they load from the sidecar of the first photo opened into an empty roll. Every change saves the active photo's sidecar 1 s after the last edit. Missing LUT files are skipped with one toast.

- [ ] **Step 1: Failing tests**

```rust
#[test] fn sidecar_round_trips()             // save then load → equal Sidecar (tempdir)
#[test] fn sidecar_path_is_next_to_the_file() // "/a/P1.RW2" → "/a/P1.RW2.rawvalley.json"
#[test] fn absent_sidecar_is_none()
#[test] fn corrupt_or_newer_sidecar_is_an_error() // "{" and version 99 → Err
#[test] fn nan_in_sidecar_is_rejected()       // develop.exposure NaN → Err
```

- [ ] **Step 2:** `cargo test -p rawvalley-core sidecar` → FAIL; implement; PASS.
- [ ] **Step 3:** Wire load/save in the UI with Review Focus 3 and 4 (one write-failure toast per session; a sidecar that failed to load is not written until the user edits).
- [ ] **Step 4:** GUI check: edit WB, crop and add a look; quit; reopen the RAW: same image (RMSE 0 vs the capture before quitting).
- [ ] **Step 5:** Italian strings; `make check`; commit `Sidecar persistence next to the photo`.

### Phase close

- [ ] `/code-review high` then `/simplify` on `master...HEAD`, fix, `make check`, commit; merge to `master` after the author's OK.
