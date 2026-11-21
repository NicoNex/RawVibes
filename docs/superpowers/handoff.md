# Handoff — 2026-11-16

Phase 6 (fixes and "As in camera") is **done and merged into `master`**:
- Chroma NR: edge-avoiding B3 wavelets, each band soft-thresholded at
  0–2σ (darktable denoiseprofile wavelets, Starck band σ); a colour edge at
  equal luminance changes within 4 px (was 46).
- Green fringe slider: darktable cacorrectrgb in brighten-only mode.
- Local contrast has its own cache stage after defringe + dehaze.
- As in camera = as-shot WB + RawTherapee's auto-matched tone curve
  (`core/src/histmatch.rs`, port of histmatching.cc) in the master curve,
  with Undo. The RW2 makernote's Real Time LUT (`core/src/makernote.rs`)
  is offered by a toast: "Use LUT" (V-Log input, its opacity, replaces
  the matched curve) or "Find…" (remembered by LUT name).
- README rewritten with fresh screenshots (test photo:
  `~/HDD/foto/Specchiulla 06-2026/P1093804.RW2`).

Open from phase 6: check As in camera, the LUT toast and the waveform
scope (garbled under a virtual display) on the real desktop. Makernote
parsing is ours (bounds-checked); rawler's TIFF API could replace it.

Roadmap: 7 undo/redo, copy/paste settings, batch export, stars + colour
labels with a filmstrip filter; 8 lensfun with automatic detection, DB
in the repo + in-app update; 9 HDR merge and HDR display export. AI
denoise: check black-level bias for the colour shift, then A/B
nonlocal-matchfilter (MIT) vs RawNIND on S9 crops before any port.

Phase 5 (export) is done and merged: header
Export = photo (Ctrl+E): JPEG with remembered quality and a size estimate
from a clean preview frame, PNG 16, TIFF 16, full resolution of the crop,
developed and graded in strips sized with the detail-tool margin, the AI
result finished first; written through a synced `.part` file, Stop keeps the
old file. The menu also lists the LUT formats (Ctrl+Shift+E = 33³ .cube).
"Not in LUT" badges skipped: the Photo / Grade tabs already split it.

Phase 5 deferred (`shortcut:` comments): export output buffers allocated per
strip; detail caches filled per strip (a no-cache develop would allow larger
strips); JPEG export holds the full 16-bit buffer plus an 8-bit copy;
encoding runs on the GPU worker (the dialog is modal).

Phase 4 (AI denoise) is done and merged. Workflow
unchanged: native execution, checkpoint commits, one `/code-review high` +
`/simplify` at the end of each phase only.

## What phase 4 delivered

- RawNIND UtNet2 (darktable's `rawdenoise-nind` weights) in our own WGSL:
  Bayer net on RGGB-packed tiles (≤ 512 px, fitted to the photo, overlap
  32, per-tile gain match), linear net for X-Trans / linear DNG. 24 MP RW2
  ≈ 2.2 s on the RTX 4070 Ti SUPER. Packaged models (PKGBUILD, dmg).
- Editor: Noise reduction → Classic | AI, Strength, progress + stop (back
  to Classic), worker runs one tile per turn.
- Defringe: port of darktable's `cacorrectrgb`.
- Slider audit (`core/src/audit.rs`): every grade and develop slider and
  every curve swept over its UI range on images with a known answer
  (identity at neutral, monotone, no dead zones, ± balanced, finite);
  `EXPECTED` lists the methods' own limits with reasons. It found and
  fixed: classic luminance NR at full strength from 1 (now darktable's
  strength = threshold scale + central weight 0.1), capture sharpening
  all in 0–10 (now 10 iterations, amount blends), grade tint reversed.
- UI: tabs **Photo** (RAW development, crop, vignette — not in the LUT) |
  **Grade** (everything in the LUT, LUTs included) | **LUT Lab**, Alt+1/2/3;
  the canvas is shared and moves between Photo and Grade. RAW development
  always visible, greyed until a RAW opens. Crop keeps its framing on a
  new ratio. LUT browser no longer jumps to the panel top. Slider value
  editor styled as an accent pill.
- Measurement harness (ignored tests in `core/src/ai/tile.rs`):
  `ai_vs_classic` (PSNR per band, bias, std on a synthetic scene at three
  noise levels), `ai_crops` (1:1 side-by-side crops of a real RAW:
  `RAWVIBES_RAW`, `RAWVIBES_OUT`).

## Open (not blocking)

- **AI colour shift** on saturated colours: R +16–27 % in greens, −13 % in
  blues (synthetic); white-balancing the input fixes greens only. Not the
  net's demosaic (re-mosaic + RCD gives the same). Cause unknown.
- Redo the AI vs classic comparison now that classic luminance NR is fixed.
- LUT intensity exists per applied LUT (Grade → LUTs rows); the browser
  preview has none — ask the author if that is what was missing.
- Deferred minors: defringe support 32 px vs 45 px reach; UNet::run
  reallocates per tile; defringe allocates per develop; local contrast
  view vs fit ~2e-3; failed model load cached until restart; CA toggle
  re-runs the AI; dehaze +100 very strong without sky.
- Not run: `makepkg` and the model converter in the packaging path (no
  `onnx` on the host).

## Next

Phase 7 (see roadmap above).
