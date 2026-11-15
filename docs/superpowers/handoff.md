# Handoff — 2026-11-15

Phase 4 (AI denoise) is **done and merged into `master`**. Workflow
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
  `RAWVALLEY_RAW`, `RAWVALLEY_OUT`).

## Open (not blocking)

- **AI colour shift** on saturated colours: R +16–27 % in greens, −13 % in
  blues (synthetic); white-balancing the input fixes greens only. Not the
  net's demosaic (re-mosaic + RCD gives the same). Cause unknown.
- **Chroma NR bleeds colour** ~20 px across saturated edges (synthetic);
  the audit does not measure edge bleed yet.
- Redo the AI vs classic comparison now that classic luminance NR is fixed.
- LUT intensity exists per applied LUT (Grade → LUTs rows); the browser
  preview has none — ask the author if that is what was missing.
- Deferred minors: defringe support 32 px vs 45 px reach; UNet::run
  reallocates per tile; defringe allocates per develop; local contrast
  view vs fit ~2e-3; failed model load cached until restart; CA toggle
  re-runs the AI; dehaze +100 very strong without sky.
- Not run: `makepkg` and the model converter in the packaging path (no
  `onnx` on the host).

## Next: phase 5, the last

Photo export (JPEG with remembered quality and live size estimate, PNG 16,
TIFF 16, full resolution) and LUT export (`.cube` 33/65/129, `.vlt`, HALD —
Grade + looks only, "not in LUT" badges on excluded sections). Export must
use the AI chain when the photo has AI on. Deferred by the user: a
local-contrast cache.
