# Phase 3 — Detail tools Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The spatial development tools of the spec, on the GPU, in the
linear Rec.2020 working space: classic noise reduction (luminance and
chrominance), capture sharpening, defringe, dehaze (± haze), local
contrast, soft light, lens vignetting; plus the creative vignette in
Finish. All excluded from LUTs, all saved in the sidecar.

**Architecture:** The develop pass of phase 2 splits in two at the working
space: `develop_linear` samples the visible region at output size into a
linear Rec.2020 f32 buffer (WB, matrix, exposure, lens vignetting), then
the detail stages run on that buffer in the spec's order (noise reduction →
defringe → dehaze → local contrast → sharpening → soft light), then
`develop_encode` does gamut compression and the profile encode into the
`GpuPhoto` the grade reads, as today. Spatial radii are given in
full-resolution pixels and scaled by the output's pixels-per-image-pixel,
so a fitted preview approximates the full-resolution result; zoomed views
sample a margin as wide as the largest filter support and crop it after.
The output of the slow stages (noise reduction, dehaze, local contrast) is
cached per slot and re-run only when their parameters or anything upstream
change. The creative vignette runs in the grade shader after the look
LUTs (not in the LUT export, which bakes per-pixel colour only).

**Tech Stack:** Rust 2024, wgpu 30 (WGSL compute), relm4 0.11 / GTK4 / libadwaita.

**Spec:** `docs/superpowers/specs/2026-11-14-rawvalley-design.md`
(Goals → RAW development; Architecture → Image pipeline, Resolution and
fluidity, Algorithms; UI).

## Global Constraints

- English strings, Italian `msgstr` in `po/it.po` in the same commit (`make pot`, then fill; `scratchpad po_fill.py` pattern).
- `make check` passes at every commit; commit trailer as in phase 1.
- Laggy sliders are an anti-goal: a detail slider drag stays interactive on a 24 MP RAW (drafts of the Preview while editing preference apply: Fast also halves the noise-reduction search window).
- Every stage at its neutral value is the identity (bit-exact passthrough, not "close").
- Develop never enters a LUT; the creative vignette neither.
- GPU tests skip with "no GPU, skipped"; sample-file tests use `RAWVALLEY_RAW` and skip without it.
- Code review + simplify once, at the end of the phase.

## Review Focus

1. Fit vs 100% consistency: the same settings look alike fitted and zoomed (radii scaled, NR strength not doubled by the downscale already averaging noise).
2. Zoomed view edges: no seams or dark borders where the margin is cropped.
3. Extreme values: NR 100 + sharpening 100 + local contrast 100 together produce no NaN, no ringing beyond 5% overshoot on a step edge.
4. Sidecars from phase 2 (version 1, no detail fields) load with every detail tool neutral.
5. Drafting: a detail slider drag never stalls the main thread (all work on the GPU worker, latest wins).

---

### Task 1: Linear stage framework

**Files:**
- Modify: `core/src/raw.wgsl` (split `develop` into `develop_linear` and `develop_encode`), `core/src/raw.rs` (`develop_into` runs linear → stages → encode; a linear scratch buffer per slot), `core/src/develop.rs` (`Detail` params)
- Modify: `core/src/sidecar.rs` (version 2: `detail`, default neutral)

**Interfaces:**
- Produces: `#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)] pub struct Detail { pub nr_luma: f32, pub nr_chroma: f32, pub nr_detail: f32, pub sharpen: f32, pub sharpen_radius: f32, pub defringe: f32, pub dehaze: f32, pub local_contrast: f32, pub soft_light: f32, pub lens_vignette: f32 }` (amounts 0–100, dehaze −100…100, `sharpen_radius` 0.5–2.0 px, default 0.8); `Develop` gains `pub detail: Detail` (`#[serde(default)]`).
- `fn stages(&self, slot, scale: f32, detail: &Detail)` in `raw.rs`: runs only the stages whose amount is non-zero; `scale` = output pixels per full-resolution pixel.
- Zoom margin: `pub fn support(detail: &Detail) -> f32` (full-resolution pixels), used by the worker to grow the view region and crop after encode.

- [x] **Step 1: Failing tests** `split_develop_matches_phase2` (every phase-2 develop test still passes through linear + encode with `Detail::default()`), `neutral_detail_is_identity` (bit-exact), `sidecar_v1_loads_with_neutral_detail`.
- [x] **Step 2:** FAIL; implement; PASS; `make check`; commit `Linear stage framework for detail tools`.

### Task 2: Lens vignetting and creative vignette

**Files:** `core/src/raw.wgsl` (in `develop_linear`), `core/src/grade.wgsl` + `core/src/grade.rs` (`Params`: `vignette`, `vignette_mid`, `vignette_round`, `vignette_feather` — finish-only, zeroed by `Grade::bake`/LUT export), `gtk/src/main.rs`

**Interfaces:** lens: gain `1 + a·r²·(1 + r²)` with `r` the distance from the sensor centre normalised to the half-diagonal (a ∈ −1…1 from the slider ±100); creative: smoothstep falloff from `mid` with `roundness` (1 = circle, 0 = frame aspect) and `feather`, multiplied in display space after the looks.

- [x] **Step 1: Failing tests** `lens_vignette_zero_is_identity`, `lens_vignette_is_radially_symmetric`, `creative_vignette_not_in_baked_lut` (bake with vignette 50 equals bake with 0).
- [x] **Step 2:** FAIL; implement; PASS; UI: Develop · Lens vignetting (one slider), Finish · Vignette (amount, midpoint, roundness, feather); Italian strings; commit `Lens and creative vignetting`.

### Task 3: Noise reduction (classic)

**Files:** `core/src/denoise.wgsl` (new), `core/src/raw.rs` (noise estimate at load, stage dispatch), `core/src/develop.rs` (`NoiseModel`)

**Interfaces:**
- `pub struct NoiseModel { pub a: f32, pub b: f32 }` — Poisson–Gaussian variance `σ² = a·x + b` of the as-shot camera RGB, estimated at load from the median absolute deviation of a high-pass of the flattest 5% of 16×16 blocks of mip level 1 (Foi et al. style), stored in `GpuRaw`.
- Luminance: generalised Anscombe transform with `(a, b)`, non-local means (patch 5×5, search 15×15 at 100% scale, 9×9 at Fast drafts), strength `h = nr_luma/100 · σ`, inverse GAT (closed-form unbiased approximation), blended back by `nr_detail`.
- Chrominance: opponent space (Y, Cb, Cr linear), edge-aware à-trous wavelets (5 levels, B3-spline, Y-guided weights), shrinkage by `nr_chroma`.
- Downscaled previews: σ scaled by `sqrt(scale)` (box averaging already removed noise).

- [x] **Step 1: Failing tests** `nr_preserves_the_mean` (synthetic Poisson–Gaussian flat field: mean within 0.5%), `nr_reduces_noise` (std ≥ 3× lower at 100), `nr_keeps_edges` (step edge 10–90% rise width grows ≤ 1 px), `noise_model_recovers_synthetic_parameters` (a, b within 15%), `chroma_nr_leaves_luminance` (Y unchanged within 1e-3).
- [x] **Step 2:** FAIL; implement; PASS; sample test prints the S9 model and timing at 2048 px; UI: Develop · Noise reduction (Luminance, Chrominance, Detail); Italian strings; commit `Classic noise reduction`.

### Task 4: Capture sharpening and defringe

**Files:** `core/src/detail.wgsl` (new), `core/src/raw.rs`

**Interfaces:**
- Sharpening: Richardson–Lucy, Gaussian PSF of `sharpen_radius` (scaled), iterations `1 + sharpen/20`, on luminance only, blended by a detail mask (local contrast of Y above `noise σ · 4` → 1, below `σ · 2` → 0) so flat areas and noise are left alone.
- Defringe: pixels within 3 px of an edge (Sobel on Y above threshold) whose hue lies in the purple (270–330°) or green (100–160°) bands are desaturated towards the 7×7 average chroma, by `defringe`.

- [x] **Step 1: Failing tests** `sharpening_raises_edge_acutance` (blurred step: 10–90% width shrinks ≥ 25% at 100), `sharpening_leaves_flat_noise` (flat noisy field std grows ≤ 5%), `sharpening_overshoot_is_bounded` (≤ 5%), `defringe_cleans_purple_edges_only` (purple fringe on an edge loses ≥ 70% chroma; a purple flat patch keeps ≥ 95%).
- [x] **Step 2:** FAIL; implement; PASS; UI: Develop · Detail (Sharpening, Radius, Defringe); Italian strings; commit `Capture sharpening and defringe`.

### Task 5: Dehaze, local contrast, soft light

**Files:** `core/src/tone.wgsl` (new), `core/src/raw.rs`

**Interfaces:**
- Dehaze: dark channel prior (min over 15 px scaled, then RGB min), atmospheric light from the brightest 0.1% of the dark channel, transmission `t = 1 − ω·dark/A` refined by a guided filter (radius 30 px scaled, ε 1e-3) on Y, `J = (I − A)/max(t, 0.1) + A`; amount scales ω; negative amounts blend towards `A` (adds haze).
- Local contrast: local Laplacian approximation (Aubry et al. fast local Laplacian, 4 intensity samples, 6 pyramid levels) on log Y, detail boost `1 + local_contrast/100`, colour ratios kept.
- Soft light: RawTherapee soft light of the image with itself on display-referred Y (`2ab + a²(1−2b)`), strength `soft_light/100`.

- [x] **Step 1: Failing tests** `dehaze_zero_is_identity`, `dehaze_raises_contrast_of_hazy_gradient` (synthetic haze: std up), `negative_dehaze_lowers_contrast`, `local_contrast_keeps_the_mean` (Y mean within 1%), `local_contrast_has_no_halo` (step edge overshoot ≤ 5%), `soft_light_zero_is_identity`, `soft_light_raises_midtone_contrast`.
- [x] **Step 2:** FAIL; implement; PASS; UI: Develop · Dehaze (−100…100), Local contrast, Soft light; Italian strings; commit `Dehaze, local contrast and soft light`.

### Task 6: Caching, zoom margins, drafts

**Files:** `core/src/raw.rs`, `core/src/gpu.rs`, `gtk/src/main.rs`

**Interfaces:** per slot (`raw_fit`, `view`), cache the linear buffer after noise reduction and after local contrast, keyed by (region, size, develop up to that stage); a WB or exposure change re-runs everything, a sharpening change only from sharpening on. Views grow by `support()` and crop it. Fast drafts halve NR search and skip local contrast's finest level.

- [x] **Step 1: Failing tests** `cached_stage_is_reused` (second develop with only `sharpen` changed runs no NR pass — counted by a debug counter), `view_matches_fit_crop` (a 1:1 view of a region equals the full-resolution develop cropped, within 2e-3, with detail tools on), `fit_and_zoom_agree_statistically` (sample RAW, NR 50: noise std of a flat patch within 25% between fit and 100%).
- [x] **Step 2:** FAIL; implement; PASS; sample test prints drag-frame time at 2048 px with all tools on (target ≤ 16 ms Balanced); commit `Detail stage caching and zoom margins`.

### Phase close

- [x] `/code-review high` then `/simplify` on the phase diff, fix, `make check`, commit; merge after the author's OK (GUI check of every section).

### Closed 2026-11-15

Deferred: local contrast shares the tone cache with defringe and dehaze, so a
local-contrast drag re-runs both (≈ 7 ms vs 3.5 ms per 1536 px frame on an RTX
4070 Ti SUPER). Its own cache costs one more full linear buffer (~100 MB at a
100% view); decide later.
