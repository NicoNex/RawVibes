# Handoff — 2026-11-15

Branch `phase4-ai-denoise` (base `master`): **phase 4 complete**, reviewed
(`/code-review high` + `/simplify`) and fixed; `make check` green.
**Not merged:** waiting for the author's GUI check.

## GUI check to do

- Defringe (darktable `cacorrectrgb` port) on a real RW2.
- Noise reduction → Classic | AI: progress bar with stop button, preview
  switching to AI when ready (24 MP RW2 ≈ 2.2 s on the RTX 4070 Ti SUPER),
  Strength slider, Chrominance still active, Luminance/Detail hidden.
- AI on a photo it cannot denoise (models missing): inline message, and
  again after Classic → AI.
- Look for faint brightness steps between AI tiles (per-tile gain match,
  as darktable does) on high-ISO shots with deep shadows.
- `make arch`: the package must now include `share/rawvalley/models/rawnind/`
  (PKGBUILD fetches the weights; needs `python-onnx`, `python-numpy`).

## Phase 4 decisions (rulings)

- Gain match weights each Bayer G plane by ½ (plain means shifted an
  unbalanced grey ~10 %).
- The worker runs one AI tile per loop turn; tiles ≤ 512 px (~300 MB of
  activations, ~60 ms per tile), fitted to the photo to cut overlap.
- Stop button = back to Classic. Classic luminance NR stays until the AI
  result is ready.
- Linear variant (X-Trans, linear DNG): `cam2rec2020 · (px / as_shot)` in,
  pseudo-inverse and `as_shot` out. Not checked on a real X-Trans file.
- Task 6 target met by tile fitting, not a faster conv kernel: a 2×2-pixel
  register-blocked `conv3` measured slower (351 vs 266 ms per 1024 tile).

## Deferred minors

- Defringe support 32 px vs the 45 px reach of its three blurs (tail
  below 2e-3 in a view-vs-fit test); sigma floor at heavy downscale.
- `UNet::run` reallocates buffers and bind groups per tile; defringe
  allocates three full-size buffers per develop; defringe Gaussian weights
  recomputed per tap.
- Local contrast: view vs fit differs ~2e-3 on a tinted edge (phase 3).
- A failed model load stays cached until restart.
- Switching CA correction drops the AI result (re-runs it).

## Next: phase 5, the last

Photo export (JPEG with remembered quality and live size estimate, PNG 16,
TIFF 16, full resolution) and LUT export (`.cube` 33/65/129, `.vlt`, HALD —
Grade + looks only, "not in LUT" badges on excluded sections). Export must
use the AI chain when the photo has AI on. Deferred by the user: a
local-contrast cache.
