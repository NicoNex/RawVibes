# Lens corrections and perspective — design

## Why

The Lens section today has only lens vignetting and automatic CA. The user shoots both native AF
lenses and vintage manual lenses (e.g. Helios 44-2 on a Panasonic DC-S9) and wants a complete,
professional lens section that works for everyone, plus perspective correction in its own section
next to the crop. RAW first; perspective must later also work on JPEG/TIFF sources.

## Goals

1. **Lens section (RAW)**
   - **Profile** from the lensfun database: found automatically from EXIF (camera make/model, lens
     model, focal length, aperture, focus distance); or chosen by hand (searchable maker → model),
     with focal length and aperture typed in when EXIF has none (manual lenses).
     Profile row shows what is applied: "Canon EF 16-35mm f/2.8L USM · auto", "None found", "Manual".
   - Profile amounts: **Distortion** 0–100 % (default 100), **Vignetting** 0–100 % (default 100),
     **Chromatic aberration** on/off (lensfun TCA).
   - **Manual corrections** (always available, on top of the profile): distortion −100…100
     (barrel ↔ pincushion), vignetting amount −100…100 (existing `lens_vignette`) + midpoint,
     CA red/cyan and blue/yellow −100…100, automatic CA (existing `ca_correct`),
     purple / green defringe (moved here from Detail, as in Lightroom).
2. **Perspective section** (own section right after Crop, with its own eye):
   - Sliders: Vertical, Horizontal (−100…100, keystone), Aspect (−100…100), Scale (50–150 %),
     Offset X / Y (−100…100). Rotation stays the crop's Straighten.
   - **Guided**: draw 2 to 4 lines on the photo (lines meant to be vertical and/or horizontal);
     the vertical/horizontal/rotation that makes them so is solved and written into the sliders.
   - **Constrain crop** switch (default on): the corrected image is scaled so it fills the frame,
     no empty corners; off shows the whole corrected image with transparent/black corners.
3. **Raster sources** (phase 2): perspective (and manual distortion) on JPEG/TIFF, same controls.

## Non-goals (now)

- Auto upright (automatic line detection), embedded makernote/DNG-opcode corrections, lensfun
  user-added profiles, editing the database. Each is a later addition on the same pipeline.
- Lens profile corrections on raster sources (camera JPEGs are usually corrected already).

## Anti-goals

- Corrections that are not lensfun's: the profile maths is lensfun's, ported, and checked against
  liblensfun on known answers. No home-grown approximations (project rule).
- A preview that stutters while dragging a slider: geometry runs per pixel on the GPU.
- A crop that silently leaves the corrected image.

## Approaches

1. **Port lensfun's models to the GPU; database converted to JSON at build** — *picked*.
   Distortion ptlens/poly3/poly5, TCA linear/poly3, vignetting pa, focal/aperture/distance
   interpolation and crop-factor rescaling as lensfun 0.3.4 does them. A Python script (like
   `convert_rawnind.py`) turns the XML database into JSON; serde_json reads it. No new Rust crate,
   real-time on the GPU.
2. liblensfun via FFI: identical behaviour, but CPU remapping (slow on 24 MP drags), a system
   library dependency, harder macOS bundle.
3. Embedded corrections only: AF lenses of some makers only. Later addition to 1.

## Decisions

- **Pipeline (RAW)**, inside `develop_linear` (core/src/raw.wgsl), inverse mapping per output pixel:
  output pixel → crop rotation (existing) → inverse perspective (homography) → inverse lens
  distortion (profile × amount, then manual) → per-channel TCA radii for R and B → `sensor()` →
  bilinear sample (three samples when TCA is on) → profile vignetting gain (× amount) and manual
  vignetting. Order follows darktable (lens before perspective in the forward pipe).
- **Perspective is geometry of the crop**: its parameters live in `Crop` (Copy, saved with the
  crop, applies to every source), with a new `Off::Perspective` eye. Lens values live in
  `Develop.detail` (RAW only). The chosen profile (strings) lives in a new sidecar field
  `lens_profile: Option<LensChoice { maker, model, focal, aperture }>` (`None` = automatic).
  `Develop` stays `Copy`.
- **Crop coordinates** stay normalised to the frame. With Constrain crop on, the corrected image
  is auto-scaled to cover the frame (lensfun's `GetAutoScale` approach for distortion, the
  homography's inner rectangle for perspective), so `crop::fits` is unchanged. With it off, the
  scale is 1 and corners outside the image render black.
- **Database**: generated at build from `/usr/share/lensfun/version_1` (Linux, `lensfun` as
  makedepends) or Homebrew's on macOS, into `target/lensfun.json`; installed to
  `share/rawvibes/lensfun.json`. Licence CC-BY-SA 3.0, noted in `models/README.md`-style notice
  and the package licences. Missing database = profile row says so; manual corrections still work.
- **Verification**: a small C harness (dev only, not shipped) against the installed liblensfun
  produces reference coordinates / gains for a set of lenses, focal lengths and apertures; the
  Rust port must match within 1e-4 (normalised). Sweep tests on distortion inversion and on the
  guided-lines solver (synthetic converging lines → recovered keystone).
- **UI order** in the Photo tab: Lens, Crop, Perspective, White balance, … (geometry first).

## Phases

1. Lens profile + manual lens corrections (RAW), database tooling, known-answer tests.
2. Perspective (manual sliders, constrain crop) on RAW.
3. Guided perspective (lines).
4. Perspective + manual distortion on raster sources.

Each phase ships on its own.

## Left to the builder

Exact JSON layout, the search widget for the profile picker, how many reference cases the C
harness covers, the guided solver's numerical method (least squares on line directions).
