# RawValley — design

Date: 2026-11-14. Status: draft for approval.

## Intent

RawValley is a professional RAW photo editor for the Linux desktop (macOS
too), born as the continuation of VibingLUT. The author shoots a Lumix S9
and currently uses RapidRaw, whose interface they find unusable. They like
VibingLUT's organisation: collapsible sections, each with an eye toggle to
bypass it, before/after split, LUT library with fast previews, and LUT
export to the camera. RawValley adds a full RAW development stage in front
of VibingLUT's grading, so one app goes from RW2 to finished photo, and from
a look built on a photo to a LUT loaded into the S9.

Success: the author opens S9 RAWs in RawValley instead of RapidRaw, starts
from a rendering that matches the camera JPEG with one click, finishes the
photo with state-of-the-art tools, and exports both the photo and a LUT that
works in-camera.

## Goals

- All RAW formats rawler decodes; Lumix S9 (RW2) is the priority and is
  tuned and tested first.
- Everything VibingLUT does today keeps working (Grade, Compare LUTs, LUT
  Lab, presets, reference match, filmstrip, scopes, `.cube`/`.vlt`/HALD).
- RAW development, state-of-the-art algorithms:
  - demosaicing (RCD for Bayer; rawler's Markesteijn path for X-Trans),
  - highlight reconstruction (inpaint opposed),
  - white balance from camera (as shot), presets, picker, or temp/tint,
  - camera colour matrix,
  - noise reduction for luminance and for chrominance, classic and AI,
  - dehaze (haze removal and haze compensation, i.e. negative amount adds haze),
  - local contrast,
  - soft light (RawTherapee "Soft Light"-style),
  - sharpening (capture/output),
  - defringe (purple/green fringe removal),
  - vignetting (lens compensation and creative),
  - crop: manual plus ratio presets: Original, 1:1, 5:4, 4:5, 4:3, 3:4,
    3:2, 2:3, 16:9, 9:16, 21:9, 2.39:1, XPan 65:24, 6:17; straighten angle.
- "As in camera" button in every section where it applies: white balance
  from the RAW metadata; tone curve and colour fitted to the embedded camera
  JPEG (reusing the reference-match engine), so opening a RAW can start
  exactly where the camera JPEG is.
- Zoom (fit, 100%, zoom in/out, pan by drag), at full resolution.
- Canvas background: black, white, or the default GTK/libadwaita colour.
- Before/after split as in VibingLUT.
- LUT preview on the photo, fast (GPU), as in Compare LUTs.
- Export of the photo (JPEG, PNG 16-bit, TIFF 16-bit) at full resolution.
  Lossy formats have a quality setting the app remembers across runs, and
  the export dialog shows an estimate of the file size that updates as the
  quality changes (builder's call: encode the fitted preview at that
  quality off the UI thread and scale by the pixel-count ratio).
- Export of professional LUTs (`.cube` 33/65/129, Panasonic `.vlt`, HALD)
  containing only what a LUT can represent (see "LUT boundary").
- GPU acceleration for all editing: wgpu with Vulkan and Metal.
- Engine and UI separated so another UI can be added later.
- UI as simple and intuitive as possible, following VibingLUT.

## Non-goals

- Library/catalogue management (DAM), ratings, keywords, tethering.
- Local adjustments with masks and brushes, healing/clone.
- Lens distortion/CA correction from lens databases (lensfun).
- HDR merge, panorama stitching, focus stacking.
- DCP/ICC camera input profiles beyond the camera matrix.

## Anti-goals

- A RapidRaw-like interface: crowded, inconsistent panels.
- A LUT that silently contains things a LUT cannot reproduce on video
  (noise reduction, sharpening, crop, …) — exported LUTs must look the same
  in-camera as in the app on the global colour level.
- Destroying the original file. Edits are non-destructive.
- Laggy sliders: an interactive slider must stay fluid on a 24 MP RAW.

## Constraints

- Rust 2024, GTK4 + libadwaita through relm4 0.11 (with relm4-macros
  `view!`, relm4-icons, relm4-components where they fit). Libraries only if
  modern, maintained and state of the art; otherwise implement ourselves.
- GPU through wgpu (Vulkan/Metal/DX12 native backends, no WebGPU/GL).
- GPL-3.0-or-later; every bundled AI model's weights must allow
  redistribution with a GPL app.
- AI model shipped inside the installation (size is not a concern; the best
  model wins).
- Commit checkpoints while developing; every ~2500 new lines (inherited
  VibingLUT code excluded), a code review round and a simplify round before
  the commit.

## Decisions made with the author

- VibingLUT is copied into this repo (history kept) and renamed; RawValley
  leads from now on. VibingLUT presets still import.
- Phased delivery: (1) base: rename, zoom/pan, background, crop;
  (2) RAW development and "as in camera"; (3) detail tools; (4) AI
  denoise; (5) photo export and LUT export.
- AI denoise runs locally on the GPU; model bundled.

## Architecture (builder's calls, stated here so they can be rejected)

### Crates

- `core` (`rawvalley-core`): engine, no GTK. Owns decoding, development
  parameters, GPU pipelines, LUT bake, export, sidecars.
- `gtk` (`rawvalley`): relm4 UI. Talks to the engine only through
  `gpu::spawn` jobs and results, as VibingLUT already does.
- `core/denoise-ai` lives in `core` behind a cargo feature `ai` (on by
  default) so a future UI or headless build can drop it.

### Image pipeline

Two parameter sets, matching the LUT boundary:

1. `Develop` (new): technical, scene-linear, may be spatial.
   `raw → black/white levels → highlight reconstruction → WB → demosaic
   → camera matrix → linear Rec.2020 working space → noise reduction (AI or
   classic) → defringe → dehaze → local contrast → sharpening → soft light`.
   Output: scene-linear image, then encoded in the selected input profile
   (Standard view transform, or V-Log for LUTs aimed at the S9), exactly as
   `Decoded::encode` does today.
2. `Grade` (VibingLUT's): global, per-pixel, display/log-referred.
   `grade → look LUTs`. This is what LUT export bakes.
3. Finishing (new, not in LUTs): crop/straighten → creative vignette →
   output colour space → encode.

Ordering of spatial tools follows darktable/RawTherapee practice: denoise
before anything that amplifies noise (dehaze, local contrast, sharpening).

### LUT boundary

Exported LUTs contain `Grade` + look LUTs only, i.e. per-pixel maps of the
input profile signal. Everything in `Develop` and Finishing is excluded.
White balance: the develop WB is excluded (the camera sets its own); the
Grade's relative WB (CAT16, already in VibingLUT) is included. The UI marks
the excluded sections with a small "not in LUT" badge so this is never a
surprise.

### Resolution and fluidity

- The full-resolution scene-linear demosaic result stays on the GPU in
  f16 (≈190 MB for 24 MP).
- Preview renders at screen resolution: when fitted, from a downsampled
  level with spatial radii scaled to it; when zoomed past it, only the
  visible tile (plus a margin for filter support) at full resolution.
- Expensive spatial stages are cached: a slider in Grade does not re-run
  denoise. Each stage re-runs only when its own parameters or an upstream
  stage change.
- Export runs the same GPU code at full resolution, tiled when the GPU's
  buffer limits require it.

### Algorithms (state of the art, per stage)

- Demosaic: RCD (Ratio Corrected Demosaicing, default in RawTherapee and
  darktable) as a WGSL compute shader. X-Trans and 4-colour sensors fall
  back to rawler's demosaic.
- Highlight reconstruction: inpaint opposed (darktable 4.x), which
  rebuilds clipped channels from opposed unclipped ones; replaces today's
  fade-to-neutral.
- Noise reduction (classic): noise level estimated per image (Poisson–
  Gaussian model, from ISO and flat regions); luminance by non-local means
  after a generalised Anscombe transform; chrominance by edge-aware à-trous
  wavelets in an opponent space. Separate luminance/chrominance amounts and
  a detail slider.
- Noise reduction (AI): NAFNet (MIT licence, state of the art on real
  noise benchmarks) exported to ONNX once, imported with `burn` and run on
  burn's wgpu backend, tiled with overlap; strength blends with the input.
  If NAFNet's weights do not survive validation (licence, quality on S9
  files), SCUNet (Apache-2.0) is the fallback.
- Dehaze: dark channel prior with guided-filter refinement (He et al.),
  in linear light; negative amounts add haze.
- Local contrast: local Laplacian filter approximation (fast, halo-free),
  as in darktable's local contrast.
- Soft light: RawTherapee's soft-light blend of the image with itself,
  strength-controlled.
- Sharpening: capture sharpening by Richardson–Lucy deconvolution with a
  Gaussian PSF (RawTherapee's capture sharpening), with a detail mask so
  flat areas and noise are left alone.
- Defringe: purple/green fringe detection by hue band near high-contrast
  edges, desaturated towards the local average.
- Vignetting: radial gain with amount, midpoint, roundness, feather; lens
  mode before the grade, creative mode after the crop.

### "As in camera"

- White balance: `rawler` as-shot coefficients.
- Tone and colour: extract the embedded JPEG preview (rawler), render the
  RAW neutrally, run `matching::transfer` between them, and load the result
  into the Grade's reference-match slot (lightness curve + a/b transfer).
  One button in each of the Tone and Colour sections; one global "Start as
  in camera" in the header.

### Persistence

Non-destructive sidecar next to the file (`<file>.rawvalley.json`), same
JSON schema family as presets (`Develop` + `Grade` + crop + looks), written
on every change after a short debounce, read when the file is opened.

### UI

VibingLUT's layout stays: header bar, image canvas, sidebar of
collapsible sections with eye toggles, scopes, filmstrip, Compare LUTs and
LUT Lab pages. Additions:

- Sidebar groups: **Develop** (White balance, Exposure & highlights,
  Noise reduction, Detail: sharpening and defringe, Dehaze, Local contrast,
  Soft light, Lens vignetting), then the existing **Grade** sections, then
  **Finish** (Crop, Vignette, Export).
- Canvas: zoom controls in an overlay (fit, 100%, −/+), scroll to zoom,
  drag to pan, double-click toggles fit/100%. Background chooser (black,
  white, default) in the view menu, remembered in settings.
- Crop mode: overlay with handles, rule-of-thirds grid, ratio dropdown,
  orientation swap, straighten slider.
- Long-running stages (AI denoise, export) show progress and can be
  cancelled.
- **LUT browsing lives in the editor, not on a page of its own** (author's
  request: VibingLUT's full-page Compare LUTs is too much for a RAW
  editor). The LUTs section of the sidebar gets a browser in the style of
  Lightroom's profile browser: a compact list/grid of thumbnails rendered
  on the current photo (search and folder filter kept), and the selected
  LUT is previewed live on the main image at once. Arrow keys (and mouse
  hover/click, and the scroll wheel over the list) step through LUTs;
  Enter or a click adds the previewed LUT to the look stack with its
  intensity; Escape leaves the photo as it was. Before/after split works
  during browsing. The Compare LUTs page is removed once this ships.

### Testing

Unit tests for each algorithm's invariants (e.g. RCD reproduces a flat
field, inpaint opposed leaves unclipped pixels untouched, denoise preserves
the mean, LUT bake excludes develop). Optional sample-file tests driven by
environment variables, as `portrait_raw_is_rotated` does today.

## Left to the builder

Exact GPU buffer layouts, tile sizes, default slider ranges, file layout
inside `core` and `gtk`, wording of labels.

## Language

UI strings are written in English in the source and translated to Italian
through gettext (`po/it.po`), as VibingLUT already does. Every new string
gets its Italian translation in the same commit.
