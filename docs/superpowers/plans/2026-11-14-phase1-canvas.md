# Phase 1 — Canvas: full resolution, zoom/pan, background, crop

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Photos are kept at full resolution and viewed with zoom/pan up to
100% and beyond, the canvas background choice persists, and photos can be
cropped (manual or ratio presets) and straightened.

**Architecture:** The engine keeps the full-resolution decode plus a mip
pyramid and resamples any rotated rectangle of it to an output size
(`core/src/view.rs`). The UI asks for the region it needs: the cropped
photo fitted to `PREVIEW_MAX` (the source of every existing page, as
today), or, when zoomed past fit, just the visible part at device-pixel
size, which replaces the Grade page's GPU photo. Crop geometry lives in
`core/src/crop.rs`, UI-free.

**Tech Stack:** Rust 2024, rawler 0.8, image 0.25, rayon, relm4 0.11 /
GTK4 / libadwaita, wgpu 30.

**Spec:** `docs/superpowers/specs/2026-11-14-rawvibes-design.md`

## Global Constraints

- Engine (`core`) has no GTK dependency; UI reaches it only through
  `gpu::spawn` jobs, `Decoded`, and pure functions.
- UI strings in English; each new string gets its Italian `msgstr` in
  `po/it.po` in the same commit.
- Crop ratio presets, in this order: Free, Original, 1:1, 5:4, 4:5, 4:3,
  3:4, 3:2, 2:3, 16:9, 9:16, 21:9, 2.39:1, XPan 65:24, 6:17.
- Canvas background: black, white, or default; remembered across runs.
- Zoom: fit, 100%, zoom in/out, scroll to zoom, drag to pan, double-click
  toggles fit/100%.
- `make check` (fmt, clippy `-D warnings`, tests) passes at every commit.
- Commit message trailer: `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`
  and `Claude-Session: https://claude.ai/code/session_01FLA4ZVXQMCf85mYpt8esUb`.

## Review Focus

1. Portrait RAWs (EXIF orientation 6/8) and portrait ratios: the crop
   presets apply to the oriented image, and "swap" turns 3:2 into 2:3.
2. Straightened crops never show areas outside the photo (no black
   corners), at any angle in −45°..45°.
3. HiDPI (scale factor 2): 100% means one image pixel per device pixel,
   and the zoomed render is sharp, not upscaled from half resolution.
4. Fast scroll-zoom and window resizing: stale region renders are dropped
   (generation counter), the newest one always wins.
5. Opening another photo while zoomed or in crop mode returns to fit and
   resets the crop; images smaller than the viewport at 100% stay at 1:1
   and centred.

---

### Task 1: Full-resolution decode with mip pyramid and region resampling

**Files:**
- Create: `core/src/view.rs`
- Modify: `core/src/photo.rs` (keep full resolution; `Decoded` gains the pyramid)
- Modify: `core/src/lib.rs` (`pub mod view;`)

**Interfaces:**
- Produces:
  - `pub struct Region { pub center: [f32; 2], pub size: [f32; 2], pub angle: f32 }`
    in `view.rs` — `center`/`size` in source pixels of the oriented image,
    `angle` in degrees, counter-clockwise, rotation about `center`.
  - `pub struct Pyramid { levels: Vec<Photo> }` with
    `pub fn new(full: Photo) -> Self`, `pub fn full(&self) -> &Photo`,
    `pub fn resample(&self, r: &Region, out_w: u32, out_h: u32) -> Photo`.
  - `Decoded::Raster(Arc<Pyramid>)`, `Decoded::Raw(Arc<Pyramid>)` (was
    `Arc<Photo>`), `Decoded::size(&self) -> (u32, u32)` (full res),
    `Decoded::region(&self, r: &Region, out_w: u32, out_h: u32) -> Decoded`
    (single-level pyramid holding the resampled photo, same variant).
  - `Decoded::encode`, `Decoded::downscaled` keep their signatures.

- [ ] **Step 1: Write the failing tests** in `view.rs`

```rust
#[test]
fn identity_region_returns_source() // 4×3 ramp, Region{center:[2.,1.5],size:[4.,3.],angle:0.}, out 4×3 → pixels equal within 1e-5
#[test]
fn downsample_preserves_mean()      // 64×64 random, full region → 8×8; mean of each channel within 1e-3
#[test]
fn rotation_90_maps_corners()       // 4×4 with a single bright pixel at (0,0), angle 90 → bright pixel lands at (0,3) (out 4×4)
#[test]
fn pyramid_halves_until_one_pixel() // 100×40 → level sizes 100×40, 50×20, 25×10, 12×5, 6×2, 3×1, 1×1
```

- [ ] **Step 2:** `cargo test -p rawvibes-core view::` → FAIL (module missing).

- [ ] **Step 3: Implement `view.rs`.** Levels by 2×2 box average (odd edge
  pixel dropped). `resample`: scale = `size.x / out_w`; pick level
  `floor(log2(scale)).clamp(0, last)`; each output pixel centre maps to the
  region (rotate by `angle` about `center`), then to that level's
  coordinates; bilinear sample with edge clamp. Output rows in parallel
  (rayon). Pixels outside the image sample the clamped edge (crop
  geometry, Task 2, keeps regions inside).

- [ ] **Step 4: Keep full resolution in `photo.rs`.** `load_raster` drops
  the `thumbnail(PREVIEW_MAX…)`; `calibrated` stops downscaling (calibrate
  per pixel, rows in parallel); `Decoded::load` wraps the result in
  `Pyramid::new`. Comment: `// shortcut: full-res f32 RGBA on the CPU (~400 MB for 24 MP); Phase 2 moves it to the GPU in f16.`
  Fix every caller of the old `Decoded::Raw(Arc<Photo>)` pattern
  (`grep -rn "Decoded::Raw\|Decoded::Raster" core gtk`).

- [ ] **Step 5:** `make check` → all pass, including the 4 new tests.

- [ ] **Step 6:** `make run FILE=<an S9 RW2>`: the photo opens and looks as
  before (Grade, Compare, Lab, filmstrip). Commit
  `Keep photos at full resolution with a mip pyramid`.

### Task 2: Crop geometry

**Files:**
- Create: `core/src/crop.rs`
- Modify: `core/src/lib.rs` (`pub mod crop;`)

**Interfaces:**
- Consumes: `view::Region`.
- Produces:
  - `pub enum Ratio { Free, Original, Fixed(f32, f32) }`
  - `pub const PRESETS: &[(&str, Ratio)]` — labels exactly as in Global
    Constraints ("XPan 65:24" → `Fixed(65., 24.)`, "2.39:1" → `Fixed(2.39, 1.)`).
  - `#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)] pub struct Crop { pub rect: [f32; 4], pub angle: f32 }`
    — `rect` = x, y, w, h normalised to the oriented image (0..1),
    describing the rectangle in the frame rotated by `angle` about the
    image centre. `Crop::default()` = full image, angle 0.
  - `pub fn fit_ratio(ratio: Ratio, img_w: u32, img_h: u32, angle: f32) -> Crop`
    — largest centred rect of that ratio inside the rotated image.
  - `pub fn clamp(c: Crop, ratio: Ratio, img_w: u32, img_h: u32) -> Crop`
    — shrinks `rect` about its centre until all 4 corners lie inside the
    rotated image, keeping the ratio.
  - `pub fn region(c: &Crop, img_w: u32, img_h: u32) -> Region`.
  - `pub fn aspect(c: &Crop, img_w: u32, img_h: u32) -> f32` (w/h in pixels).

- [ ] **Step 1: Failing tests**

```rust
#[test] fn default_is_full_image()          // region(&Crop::default(), 6000, 4000) == Region{center:[3000.,2000.],size:[6000.,4000.],angle:0.}
#[test] fn xpan_ratio_on_landscape()        // fit_ratio(Fixed(65.,24.), 6000, 4000, 0.) → aspect ≈ 65/24 (1e-3), width == full width
#[test] fn portrait_ratio_on_portrait()     // fit_ratio(Fixed(2.,3.), 4000, 6000, 0.) == full image
#[test] fn rotated_crop_stays_inside()      // for angle in [-45,-10,5,30,45]: corners of region(fit_ratio(Original,…,angle)) inside the unrotated 6000×4000 rect (1e-2 px)
#[test] fn clamp_keeps_ratio()              // clamp of an oversized 16:9 rect at 10° → aspect still 16/9, corners inside
```

- [ ] **Step 2:** `cargo test -p rawvibes-core crop::` → FAIL.
- [ ] **Step 3: Implement.** Inscribed rect of aspect `a` in a W×H rect
  rotated by θ, centred: `h = min(H / (cos|θ| + a·sin|θ|), W / (a·cos|θ| + sin|θ|))`, `w = a·h`.
  `clamp` binary-searches a scale in (0, 1] on the corner test.
- [ ] **Step 4:** `make check` → pass. Commit `Crop geometry with ratio presets and straighten`.

### Task 3: Fit preview from the crop; zoomed region rendering

**Files:**
- Modify: `gtk/src/main.rs` (App state, `encode_photo`, `Msg`, canvas widgets)
- Modify: `po/it.po`

**Interfaces:**
- Consumes: `Decoded::region`, `crop::{Crop, region, aspect}`, `Job::SetPhoto`, `Job::Render`.
- Produces (UI-internal): `App.crop: Crop`, `App.zoom: Option<Zoom>` with
  `struct Zoom { scale: f32 /* image px per device px */, center: [f32; 2] /* normalised in the crop */ }`,
  `Msg::Zoom(ZoomCmd)` with `enum ZoomCmd { Fit, Actual, In, Out, Scroll { dy: f64, at: (f64, f64) }, Pan { dx: f64, dy: f64 }, Toggle }`,
  `Msg::ViewReady(u64, Arc<Photo>)`.

- [ ] **Step 1:** `encode_photo` encodes `decoded.region(region(&crop), fit)`
  where `fit` keeps the crop's aspect within `PREVIEW_MAX` (the existing
  background thread + `load_gen` pattern). This `photo` stays the source of
  Compare, Lab, match, scopes. Rasters go through the same path now.
- [ ] **Step 2:** Zoom state. Fit = `zoom: None`. `Actual` = scale 1.0.
  In/Out step ×√2, clamped to [1/32 of fit … 1/8 (800%)]. `Scroll` zooms
  about the cursor (the image point under the cursor stays put). `Pan`
  moves `center`, clamped so the view never leaves the crop. `Toggle`
  switches Fit ↔ Actual about the double-click point.
- [ ] **Step 3:** When `zoom` is `Some`, compute the visible sub-region of
  the crop for the canvas size × `widget.scale_factor()`; if it is smaller
  than the crop, resample it on a thread (`view_gen` counter, stale results
  dropped in `Msg::ViewReady`), encode for the profile, `Job::SetPhoto` it,
  and re-render; the Picture shows it 1:1 to device pixels. When it covers
  the whole crop, or `zoom` is `None`, send the fit `photo` again. Re-run on
  canvas resize (`connect_resize` on a `gtk::DrawingArea` overlaid as the
  size probe, or `notify::width`/`height` on the Picture's parent).
- [ ] **Step 4:** Input on `grade_pic`: `EventControllerScroll` (vertical)
  → `Scroll`; `GestureClick` with `n_press == 2` → `Toggle`; drag: when
  compare is off and zoomed, primary drag → `Pan`; middle-button drag →
  `Pan` always; primary drag with compare on keeps moving the split
  (existing `add_split_drag`). Keyboard: `Ctrl+0` fit, `Ctrl+1` 100%,
  `Ctrl+plus`/`Ctrl+minus`.
- [ ] **Step 5:** Overlay at the canvas bottom-right (`osd` linked box):
  buttons `zoom-out-symbolic`, a label with the percentage (click = Fit),
  `zoom-in-symbolic`, `zoom-fit-best-symbolic`, `zoom-original-symbolic`,
  each with a tooltip and accessible label. Info label shows the full
  resolution, not the preview size.
- [ ] **Step 6:** New photo, or new active filmstrip item → `zoom = None`,
  `crop = Crop::default()`.
- [ ] **Step 7:** Italian strings in `po/it.po` ("Zoom in" → "Ingrandisci",
  "Zoom out" → "Riduci", "Fit" → "Adatta", "Actual size" → "Dimensione reale").
- [ ] **Step 8:** `make check`; `make run FILE=<S9 RW2>`: scroll-zoom to
  100% shows sharp per-pixel detail; pan by drag; double-click toggles;
  split still works with compare on; resize keeps the view. Repeat with a
  portrait RAW and with `GDK_SCALE=2`. Commit `Zoom and pan the photo at full resolution`.

### Task 4: Crop UI

**Files:**
- Create: `gtk/src/crop_ui.rs` (section widgets + overlay drawing and drag)
- Modify: `gtk/src/main.rs` (wire the section into the sidebar, after the grade sections; `Msg::Crop*`)
- Modify: `po/it.po`

**Interfaces:**
- Consumes: `crop::{Crop, Ratio, PRESETS, fit_ratio, clamp, aspect}`, `App.crop`, the Task 3 encode path.
- Produces: `CropUi { section: gtk::Box, overlay: gtk::DrawingArea }`,
  `Msg::CropEdit(bool)`, `Msg::CropRatio(usize)`, `Msg::CropSwap`,
  `Msg::CropAngle(f32)`, `Msg::CropRect([f32; 4])`, `Msg::CropReset`.

- [ ] **Step 1:** Section "Crop" built with the existing `section()` helper:
  toggle button "Edit crop" (`Ctrl+R`), ratio `gtk::DropDown` over
  `PRESETS`, swap-orientation button (`object-flip-vertical-symbolic`,
  tooltip "Swap orientation"; disabled for Free/1:1), straighten `Slider`
  −45..45° (double-click resets), Reset button. No eye toggle (a crop is
  not a look).
- [ ] **Step 2:** Edit mode: the canvas shows the whole straightened image
  fitted (zoom forced to `None`); the overlay draws the outside of the rect
  at 55% black, the rect border, rule-of-thirds lines, and 8 handles.
  Drag a corner/edge → resize (ratio kept unless Free, then `clamp`); drag
  inside → move (clamped inside); `Enter` or toggling off applies;
  `Escape` restores the crop from before edit mode.
- [ ] **Step 3:** Changing ratio or angle calls `fit_ratio` (then the user
  adjusts). Swap uses `Fixed(b, a)`; Original uses the oriented image
  aspect.
- [ ] **Step 4:** Leaving edit mode re-encodes the preview through the
  Task 3 path; the info label shows the cropped size in pixels.
- [ ] **Step 5:** Italian strings ("Crop" → "Ritaglio", "Edit crop" →
  "Modifica ritaglio", "Straighten" → "Raddrizza", "Swap orientation" →
  "Inverti orientamento", "Free" → "Libero", "Original" → "Originale",
  "Reset" already translated).
- [ ] **Step 6:** `make check`; run: each preset on a landscape and a
  portrait RW2, straighten at ±45° shows no black corners, Escape cancels,
  zoom works inside the crop afterwards. Commit `Crop and straighten`.

### Task 5: Remember the canvas background

**Files:**
- Modify: `gtk/src/settings.rs` (`pub const BACKGROUND: &str = "background";`,
  `pub fn value(key) -> Option<String>`, `pub fn set_value(key, &str) -> io::Result<()>`;
  `folder`/`set_folder` reuse them)
- Modify: `gtk/src/main.rs` (the `background` action's initial state from
  settings; `change_state` saves it)

- [ ] **Step 1:** Implement; the initial state applies the CSS class at
  startup (call `change_state` once with the stored value, default
  `"default"`; unknown values fall back to default).
- [ ] **Step 2:** `make check`; run: pick White, quit, relaunch → white.
  Commit `Remember the canvas background`.

### Phase close

- [ ] Count new lines since `f5090ce` (`git diff --shortstat f5090ce -- core gtk`).
  If ≥ 2500, or at the end of the phase, run `/code-review` then
  `/simplify` on the diff, fix, `make check`, commit `Review fixes: phase 1`.
