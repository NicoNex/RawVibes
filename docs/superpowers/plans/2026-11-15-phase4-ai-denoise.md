# Phase 4 — AI denoise Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** RawNIND UtNet2 denoising on the GPU in our own WGSL, as an alternative to classic noise reduction, bundled with the app.

**Architecture:** The ONNX models darktable ships (`rawdenoise-nind`, GPL-3.0 + CC-BY-4.0 weights) are converted offline into a flat f32 blob plus a JSON layer list. A small UtNet2 runner (`core/src/ai/`) executes the six ops with our own compute shaders on the app's wgpu device. The Bayer variant replaces RCD at RAW load for Bayer sensors (its own joint demosaic, which darktable throws away); the linear variant runs on demosaiced camera RGB converted to linear Rec.2020 for other sensors. The result becomes an alternate level 0 + mips of the `GpuRaw`, so every downstream stage, view and cache works unchanged.

**Tech Stack:** Rust, wgpu 30 / WGSL, rayon; Python 3 + `onnx` only in the offline conversion script.

**Spec:** `docs/superpowers/specs/2026-11-14-rawvalley-design.md` (Algorithms · Noise reduction (AI); UI · long-running stages). Feasibility report: burn-onnx imports both models; ops are Conv 3×3 (pad 1) ×18 + 1×1 ×1, LeakyRelu α 0.2 ×18, MaxPool 2×2 s2 ×4, ConvTranspose 2×2 s2 ×4, Concat axis 1 ×4, Bayer only DepthToSpace(2, CRD). ~7.76 M params; FP32 required (raw outputs reach 1e9).

## Global Constraints

- Weights source: `https://github.com/darktable-org/darktable-ai/releases/download/release-5.6.0/rawdenoise-nind.dtmodel`, sha256 `d71b5f1e727c85a359e6f74dca9e2016c9d8fc3e2f7ac3e9b347d80ceca969af`; credit Brummer & De Vleeschouwer (arXiv 2501.08924), link the original checkpoints, ship the conversion script (GPL "preferred form" of the weights).
- Behind cargo feature `ai` in `core`, on by default; without it the method row is hidden.
- No new runtime dependencies. FP32 throughout the network.
- Never blocks the GTK main thread; long runs show progress and can be cancelled (spec UI).
- Preprocessing as darktable/rawnind: per-CFA-site `(v − black)/(white − black)` (no clip), RGGB-aligned packing `[R, G(row0), G(row1), B]`, mirror padding inside the RGGB-aligned area, white balance none (manifest `wb_norm: none`), tiles with core written back and overlap 32 packed pixels, per-tile gain match `out *= mean(in)/mean(out)` (skip when |mean(out)| < 1e-6·|mean(in)|).
- Code review + simplify once at phase end; UI work loads `gtk-ui-ux-engineer` + `frontend-design`, Rust work `rust-engineer`.

## Review Focus

1. Tile seams: a smooth ramp through the tiled runner shows no step at tile borders (≤ 1% of local value) — test in Task 3.
2. Dark or out-of-distribution input (deep shadows, black frames): no NaN/Inf, no ±1e10 blow-ups reaching the image (gain match guarded, output clamped ≥ 0) — test in Task 3.
3. Cancel mid-run, or open another photo mid-run: no stale result applied to the new photo, GPU memory freed — test in Task 4 (generation check) .
4. GPU memory limits (iGPU, 2 GB): tile size picked from `fits()`, never a panic — test in Task 3 (forced small limit).
5. Sidecars without the new fields load as Classic NR; AI off is bit-exact with today — test in Task 4.

---

### Task 1: Models: fetch, convert, load

**Files:**
- Create: `tools/convert_rawnind.py`, `models/README.md` (credits, licence, source links)
- Modify: `Makefile` (`models` target: download to `target/models/`, verify sha256, unzip, run converter into `models/rawnind/`; `install` copies `models/rawnind/*` to `$(PREFIX)/share/rawvalley/models/rawnind/`), `.gitignore` (`models/rawnind/`)
- Create: `core/src/ai/mod.rs`, `core/src/ai/weights.rs`; Modify: `core/Cargo.toml` (`[features] default = ["ai"]`, `ai = []`), `core/src/lib.rs`

**Interfaces:**
- Produces: converter writes `models/rawnind/{bayer,linear}.bin` (little-endian f32, layers in graph order) and `{bayer,linear}.json`: `{"layers":[{"op":"conv"|"convT"|"pool"|"concat"|"shuffle","name":..,"in":c,"out":c,"k":3|2|1,"offset":floats,"skip":layer_index?}],"input_channels":4|3,"output_channels":3,"scale":2|1}`; plus `{bayer,linear}_ref.bin`: a 64×64 (packed) deterministic input and the ONNX reference output (`onnx.reference.ReferenceEvaluator`).
- Produces: `pub struct Weights { pub graph: Vec<Layer>, pub data: Vec<f32> }`, `pub fn Weights::load(dir: &Path, variant: Variant) -> Result<Weights, String>`, `pub enum Variant { Bayer, Linear }`, `pub fn models_dir() -> Option<PathBuf>` (env `RAWVALLEY_MODELS`, then `<exe>/../share/rawvalley/models`, then the repo `models/` in debug builds).

- [x] **Step 1: Failing tests** `weights_load_both_variants` (skips with "no models, skipped" when `models_dir()` is None): 19 convs, 4 pools, 4 convT, 4 concats, parameter count within 1% of 7.76 M, every `offset + size` ≤ `data.len()`, channel counts chain.
- [x] **Step 2:** write converter + loader; `make models` then PASS; commit `AI models: fetch, convert, load`.

### Task 2: UtNet2 on the GPU

**Files:** Create `core/src/ai/unet.rs`, `core/src/ai/unet.wgsl` (prelude `common.wgsl`)

**Interfaces:**
- Consumes: `Weights`.
- Produces: `pub struct UNet` (weights uploaded once, pipelines), `UNet::new(gpu: &Gpu, w: Weights) -> UNet`, `UNet::run(&self, gpu: &Gpu, input: &wgpu::Buffer, w: u32, h: u32) -> wgpu::Buffer` (planar CHW f32; `w`, `h` multiples of 16; output `3 × (w·scale) × (h·scale)`). Kernels: `conv3x3_lrelu` (fused LeakyReLU 0.2, bias), `conv1x1`, `convT2x2`, `maxpool2`, `shuffle2` (CRD); concat = channel offsets into one buffer, no copy.

- [x] **Step 1: Failing test** `unet_matches_onnx_reference`: both variants, the `_ref.bin` input → max |Δ| ≤ 1e-4 · max|ref| and no NaN.
- [x] **Step 2:** first a direct, correct version (one invocation per output pixel × 8 output channels, input tile in workgroup memory per 8-input-channel chunk); PASS; commit `UtNet2 on the GPU`.

### Task 3: Tiled denoise of a RAW

**Files:** Create `core/src/ai/tile.rs`; Modify `core/src/raw.rs` (Bayer packing pass `ai_pack`, mirror pad, `ai_unpack` into a level-0 camera-RGB buffer in the same packed layout `rcd_pack` writes)

**Interfaces:**
- Produces: `pub fn Gpu::ai_denoise(&self, raw: &RawData, net: &UNet, cancel: &AtomicBool, progress: &dyn Fn(f32)) -> Result<Option<wgpu::Buffer>, String>` — `Ok(None)` when cancelled; tile edge picked as the largest of 1024/512/256 packed px whose activations pass `fits()`; output clamped ≥ 0.
- [x] **Step 1: Failing tests** `tiles_have_no_seams` (ramp mosaic over 3×3 tiles at forced 256-px tiles: max jump across a tile border ≤ 1% of the local value), `ai_keeps_the_mean` (flat noisy mosaic: mean within 1%), `ai_reduces_noise` (synthetic Poisson–Gaussian flat: std ≥ 3× lower), `dark_input_stays_finite` (all-black and 1e-4 mosaics: finite, ≥ 0), `cancel_returns_none`, `small_gpu_limit_uses_small_tiles`.
- [x] **Step 2:** implement; PASS; sample test `ai_on_sample` (RAWVALLEY_RAW) prints time for a 24 MP RW2; commit `Tiled AI denoise of a RAW`.

### Task 4: AI noise reduction in the editor

**Files:** `core/src/develop.rs` (`Detail { nr_ai: bool, nr_ai_strength: f32 }`, default false / 100; support unchanged), `core/src/gpu.rs` (worker keeps the RAW's `RawData` and a `Job::AiDenoise { generation }`; result stored as the `GpuRaw`'s alternate level chain; `Done::AiProgress(f32)`, `Done::AiReady(generation)`), `core/src/raw.rs` (`develop_into` samples the AI chain blended with RCD by strength when `nr_ai`), `gtk/src/main.rs`, `po/`.

- UI (Noise reduction section): a two-way toggle "Classic | AI" at the top; AI shows "Strength" and hides Luminance/Detail (chrominance stays: AI leaves some colour noise); while running, an inline progress bar with a cancel button in the section; preview keeps the classic result until AI is ready; a failed or missing model shows an inline message with the reason.
- [x] **Step 1: Failing tests** `ai_off_is_bit_exact`, `ai_strength_zero_equals_rcd`, `sidecar_without_ai_fields_is_classic`, `stale_ai_result_is_dropped` (generation).
- [x] **Step 2:** implement; PASS; Italian strings; commit `AI noise reduction in the editor`.

### Task 5: Linear variant for other sensors

**Files:** `core/src/ai/tile.rs`, `core/src/raw.rs`
- X-Trans / RGB raws: demosaiced camera RGB → linear Rec.2020 (no WB) → linear model → back to camera RGB → level 0.
- [x] **Step 1: Failing test** `linear_variant_reduces_noise_on_rgb_raw` (an `rgb_raw` test photo).
- [x] **Step 2:** implement; PASS; commit `AI denoise for non-Bayer sensors`.

### Task 6: Speed

**Files:** `core/src/ai/unet.wgsl`, `core/src/ai/unet.rs`
- Tuned conv: implicit GEMM with register blocking (each invocation 4×4 pixels × 8 channels), shared input tiles; subgroup ops where the adapter has them (`Features::SUBGROUP`), plain path otherwise.
- [x] **Step 1:** `unet_matches_onnx_reference` still passes; sample timing target on the RTX 4070 Ti SUPER: 24 MP RW2 ≤ 2.5 s (burn baseline 6–7 s).
- [x] **Step 2:** commit `Faster UtNet2 convolutions`.

### Phase close

- [x] `/code-review high` then `/simplify` on the phase diff, fix, `make check`, commit.
- [ ] Merge after the author's GUI check.
